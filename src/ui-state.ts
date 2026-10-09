import { isTauri } from "@tauri-apps/api/core";

export const native = isTauri();
export const preview = import.meta.env.DEV && !native;

export function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message;
  if (typeof error === "string") return error;
  return "Something went wrong. Try refreshing.";
}
