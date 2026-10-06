use std::time::Duration;

use reqwest::{Client, ClientBuilder, StatusCode, header::HeaderMap};
use serde::Deserialize;
use time::{
    OffsetDateTime,
    format_description::well_known::{Rfc2822, Rfc3339},
};

use super::UpdateError;

const LATEST_URL: &str = "https://api.github.com/repos/freonoma/delta-v/releases/latest";
const MAX_RESPONSE_BYTES: usize = 256 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Release {
    pub tag_name: String,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct FetchResult {
    pub result: Result<Release, UpdateError>,
    pub retry_at: Option<i64>,
}

fn client_builder() -> ClientBuilder {
    Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(7))
        .timeout(Duration::from_secs(20))
        .user_agent("Delta-V")
}

pub(super) async fn latest() -> FetchResult {
    let client = match client_builder().build() {
        Ok(client) => client,
        Err(_) => {
            return FetchResult {
                result: Err(UpdateError::Network),
                retry_at: None,
            };
        }
    };
    fetch(&client, LATEST_URL, || {
        OffsetDateTime::now_utc().unix_timestamp()
    })
    .await
}

async fn fetch(client: &Client, url: &str, now: impl FnOnce() -> i64) -> FetchResult {
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2026-03-10")
        .send()
        .await;
    let response = match response {
        Ok(response) => response,
        Err(_) => {
            return FetchResult {
                result: Err(UpdateError::Network),
                retry_at: None,
            };
        }
    };
    let now = now();
    let status = response.status();
    let headers = response.headers();
    let retry_after = retry_after(headers, now);
    let exhausted = header(headers, "x-ratelimit-remaining") == Some("0");
    let reset = exhausted.then(|| {
        header(headers, "x-ratelimit-reset")
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|timestamp| *timestamp > now)
            .unwrap_or_else(|| now.saturating_add(60))
    });
    let retry_at = retry_after.into_iter().chain(reset).max();
    let limited = || UpdateError::RateLimited {
        retry_at: retry_at.unwrap_or_else(|| now.saturating_add(60)),
    };
    let service = || UpdateError::Service {
        status: status.as_u16(),
        retry_at,
    };
    let result = if status == StatusCode::TOO_MANY_REQUESTS
        || (status == StatusCode::FORBIDDEN && (exhausted || retry_after.is_some()))
    {
        Err(limited())
    } else if status == StatusCode::FORBIDDEN {
        match read_body(response).await {
            Ok(body) if rate_limit_message(&body) => Err(limited()),
            Ok(_) => Err(service()),
            Err(error) => Err(error),
        }
    } else if status == StatusCode::OK {
        read_body(response)
            .await
            .and_then(|body| parse_release(&body))
    } else {
        Err(service())
    };
    let retry_at = match &result {
        Err(UpdateError::RateLimited { retry_at }) => Some(*retry_at),
        _ => retry_at,
    };
    FetchResult { result, retry_at }
}

async fn read_body(mut response: reqwest::Response) -> Result<Vec<u8>, UpdateError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(UpdateError::InvalidResponse);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| UpdateError::Network)? {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(UpdateError::InvalidResponse);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok().map(str::trim)
}

fn retry_after(headers: &HeaderMap, now: i64) -> Option<i64> {
    let value = header(headers, "retry-after")?;
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Some(now.saturating_add(value.parse::<i64>().unwrap_or(i64::MAX)));
    }
    OffsetDateTime::parse(value, &Rfc2822)
        .ok()
        .map(|date| date.unix_timestamp().max(now))
}

fn rate_limit_message(bytes: &[u8]) -> bool {
    #[derive(Deserialize)]
    struct ErrorBody {
        message: String,
    }
    serde_json::from_slice::<ErrorBody>(bytes)
        .ok()
        .is_some_and(|body| {
            let message = body.message.to_ascii_lowercase();
            message.starts_with("api rate limit exceeded")
                || message.starts_with("you have exceeded a secondary rate limit")
        })
}

fn parse_release(bytes: &[u8]) -> Result<Release, UpdateError> {
    #[derive(Deserialize)]
    struct Response {
        tag_name: String,
        draft: bool,
        prerelease: bool,
        published_at: Option<String>,
    }
    let response: Response =
        serde_json::from_slice(bytes).map_err(|_| UpdateError::InvalidResponse)?;
    if response.draft
        || response.prerelease
        || response.tag_name.is_empty()
        || response.tag_name.len() > 128
        || response.tag_name.chars().any(char::is_control)
        || !response
            .published_at
            .as_deref()
            .is_some_and(|published| OffsetDateTime::parse(published, &Rfc3339).is_ok())
    {
        return Err(UpdateError::InvalidResponse);
    }
    Ok(Release {
        tag_name: response.tag_name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread::{self, JoinHandle},
        time::Instant,
    };

    const NOW: i64 = 1_791_288_000;
    const RELEASE: &str = r#"{"tag_name":"v0.3.1","draft":false,"prerelease":false,"published_at":"2026-10-06T12:00:00Z"}"#;

    fn test_client(timeout: Duration) -> Client {
        client_builder()
            .https_only(false)
            .no_proxy()
            .timeout(timeout)
            .build()
            .unwrap()
    }

    fn serve(response: String, delay: Duration) -> (String, JoinHandle<String>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let task = thread::spawn(move || {
            let started = Instant::now();
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(connection) => break connection,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && started.elapsed() < Duration::from_secs(2) =>
                    {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) => panic!("Fixture did not accept a request: {error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let mut chunk = [0; 1024];
                let count = stream.read(&mut chunk).unwrap();
                assert!(count > 0 && request.len() < 8192);
                request.extend_from_slice(&chunk[..count]);
            }
            thread::sleep(delay);
            // Timeout and size-limit tests deliberately close the connection early.
            let _ = stream.write_all(response.as_bytes());
            String::from_utf8(request).unwrap()
        });
        (
            format!("http://{address}/repos/freonoma/delta-v/releases/latest"),
            task,
        )
    }

    fn response(status: u16, headers: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
            body.len()
        )
    }

    async fn fetch_response(response: String) -> FetchResult {
        let (url, task) = serve(response, Duration::ZERO);
        let result = fetch(&test_client(Duration::from_secs(2)), &url, || NOW).await;
        task.join().unwrap();
        result
    }

    #[test]
    fn requires_a_published_full_release() {
        assert_eq!(
            parse_release(RELEASE.as_bytes()),
            Ok(Release {
                tag_name: "v0.3.1".into(),
            })
        );
        for body in [
            RELEASE.replace("\"draft\":false", "\"draft\":true"),
            RELEASE.replace("\"prerelease\":false", "\"prerelease\":true"),
            RELEASE.replace("\"2026-10-06T12:00:00Z\"", "null"),
            RELEASE.replace("2026-10-06T12:00:00Z", "invalid date"),
            RELEASE.replace("v0.3.1", ""),
            RELEASE.replace("v0.3.1", &"v".repeat(129)),
            RELEASE.replace("v0.3.1", "v0.3.1\\n"),
            "{}".into(),
            "not JSON".into(),
        ] {
            assert_eq!(
                parse_release(body.as_bytes()),
                Err(UpdateError::InvalidResponse)
            );
        }
    }

    #[test]
    fn retry_after_accepts_seconds_and_http_dates_without_overflow() {
        let mut headers = HeaderMap::new();
        for (value, expected) in [
            ("120", Some(NOW + 120)),
            ("999999999999999999999999", Some(i64::MAX)),
            ("Tue, 06 Oct 2026 12:02:00 GMT", Some(NOW + 120)),
            ("Tue, 06 Oct 2026 11:59:00 GMT", Some(NOW)),
            ("invalid", None),
            ("-10", None),
            ("", None),
        ] {
            headers.insert("retry-after", value.parse().unwrap());
            assert_eq!(retry_after(&headers, NOW), expected, "{value}");
        }
    }

    #[tokio::test]
    async fn request_has_fixed_path_static_identity_and_no_credentials() {
        assert_eq!(
            LATEST_URL,
            "https://api.github.com/repos/freonoma/delta-v/releases/latest"
        );
        let (url, task) = serve(response(200, "", RELEASE), Duration::ZERO);
        assert!(
            fetch(&test_client(Duration::from_secs(2)), &url, || NOW)
                .await
                .result
                .is_ok()
        );
        let request = task.join().unwrap().to_ascii_lowercase();
        assert!(request.starts_with("get /repos/freonoma/delta-v/releases/latest http/1.1\r\n"));
        assert!(request.contains("\r\nuser-agent: delta-v\r\n"));
        assert!(request.contains("\r\naccept: application/vnd.github+json\r\n"));
        assert!(request.contains("\r\nx-github-api-version: 2026-03-10\r\n"));
        for forbidden in [
            "authorization:",
            "cookie:",
            "chatgpt-account-id:",
            "anthropic-beta:",
        ] {
            assert!(!request.contains(forbidden));
        }
    }

    #[tokio::test]
    async fn production_client_rejects_http_and_redirects_are_not_followed() {
        let client = client_builder().no_proxy().build().unwrap();
        assert_eq!(
            fetch(&client, "http://127.0.0.1:1", || NOW).await.result,
            Err(UpdateError::Network)
        );
        assert_eq!(
            fetch_response(response(302, "Location: /redirected\r\n", ""))
                .await
                .result,
            Err(UpdateError::Service {
                status: 302,
                retry_at: None
            })
        );
    }

    #[tokio::test]
    async fn enforces_both_advertised_and_streamed_size_limits() {
        let advertised = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            MAX_RESPONSE_BYTES + 1
        );
        assert_eq!(
            fetch_response(advertised).await.result,
            Err(UpdateError::InvalidResponse)
        );
        let chunk = "x".repeat(MAX_RESPONSE_BYTES / 2);
        let chunked = format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{chunk}\r\n{:x}\r\n{chunk}\r\n1\r\nx\r\n0\r\n\r\n",
            chunk.len(),
            chunk.len()
        );
        assert_eq!(
            fetch_response(chunked).await.result,
            Err(UpdateError::InvalidResponse)
        );
    }

    #[tokio::test]
    async fn timeout_and_truncated_body_are_network_failures() {
        let (url, task) = serve(response(200, "", RELEASE), Duration::from_millis(250));
        assert_eq!(
            fetch(&test_client(Duration::from_millis(50)), &url, || NOW)
                .await
                .result,
            Err(UpdateError::Network)
        );
        task.join().unwrap();
        assert_eq!(
            fetch_response(
                "HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{}".into()
            )
            .await
            .result,
            Err(UpdateError::Network)
        );
    }

    #[tokio::test]
    async fn error_statuses_and_bad_success_bodies_do_not_become_current() {
        for status in [401, 403, 404, 500, 503] {
            assert_eq!(
                fetch_response(response(status, "", "{\"message\":\"Access denied\"}"))
                    .await
                    .result,
                Err(UpdateError::Service {
                    status,
                    retry_at: None
                })
            );
        }
        assert_eq!(
            fetch_response(response(503, "Retry-After: 120\r\n", ""))
                .await
                .result,
            Err(UpdateError::Service {
                status: 503,
                retry_at: Some(NOW + 120)
            })
        );
        assert_eq!(
            fetch_response(response(200, "", "invalid JSON"))
                .await
                .result,
            Err(UpdateError::InvalidResponse)
        );
    }

    #[tokio::test]
    async fn rate_limits_require_evidence_and_respect_wait_headers() {
        for (status, headers, body, retry_at) in [
            (429, "".into(), "", NOW + 60),
            (429, "Retry-After: 120\r\n".into(), "", NOW + 120),
            (403, "Retry-After: 120\r\n".into(), "", NOW + 120),
            (
                403,
                format!(
                    "X-RateLimit-Remaining: 0\r\nX-RateLimit-Reset: {}\r\n",
                    NOW + 180
                ),
                "",
                NOW + 180,
            ),
            (403, "X-RateLimit-Remaining: 0\r\n".into(), "", NOW + 60),
            (
                403,
                "".into(),
                "{\"message\":\"API rate limit exceeded for an address.\"}",
                NOW + 60,
            ),
            (
                403,
                "".into(),
                "{\"message\":\"You have exceeded a secondary rate limit.\"}",
                NOW + 60,
            ),
        ] {
            assert_eq!(
                fetch_response(response(status, &headers, body))
                    .await
                    .result,
                Err(UpdateError::RateLimited { retry_at })
            );
        }
        let positive = format!(
            "X-RateLimit-Remaining: 12\r\nX-RateLimit-Reset: {}\r\n",
            NOW + 600
        );
        assert_eq!(
            fetch_response(response(403, &positive, "{}")).await.result,
            Err(UpdateError::Service {
                status: 403,
                retry_at: None
            })
        );
        assert_eq!(
            fetch_response(response(200, &positive, RELEASE))
                .await
                .retry_at,
            None
        );
        let exhausted = format!(
            "X-RateLimit-Remaining: 0\r\nX-RateLimit-Reset: {}\r\n",
            NOW + 600
        );
        assert_eq!(
            fetch_response(response(200, &exhausted, RELEASE))
                .await
                .retry_at,
            Some(NOW + 600)
        );
    }

    #[tokio::test]
    async fn wait_headers_survive_invalid_responses_and_service_failures() {
        let headers = format!(
            "X-RateLimit-Remaining: 0\r\nX-RateLimit-Reset: {}\r\n",
            NOW + 600
        );
        for (reply, error) in [
            (
                response(200, &headers, "invalid JSON"),
                UpdateError::InvalidResponse,
            ),
            (
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n{headers}\r\n{{}}"
                ),
                UpdateError::Network,
            ),
            (
                response(503, &headers, ""),
                UpdateError::Service {
                    status: 503,
                    retry_at: Some(NOW + 600),
                },
            ),
        ] {
            assert_eq!(
                fetch_response(reply).await,
                FetchResult {
                    result: Err(error),
                    retry_at: Some(NOW + 600)
                }
            );
        }
        let failed = fetch_response(response(200, "Retry-After: 120\r\n", "invalid JSON")).await;
        assert_eq!(failed.retry_at, Some(NOW + 120));
        assert_eq!(failed.result, Err(UpdateError::InvalidResponse));
        for reset in [
            "",
            "X-RateLimit-Reset: invalid\r\n",
            "X-RateLimit-Reset: 1\r\n",
        ] {
            let headers = format!("X-RateLimit-Remaining: 0\r\n{reset}");
            let success = fetch_response(response(200, &headers, RELEASE)).await;
            assert!(success.result.is_ok());
            assert_eq!(success.retry_at, Some(NOW + 60));
        }
    }
}
