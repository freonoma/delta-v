import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import { SettingsWindow } from "./SettingsWindow";
import { native, preview } from "./ui-state";
import "./styles.css";
import "./SettingsWindow.css";
import "./glass.css";

const root = document.getElementById("root");

if (!root) {
  throw new Error("The app root is missing.");
}

if (preview) document.body.classList.add("browser-preview");
if (native) document.body.classList.add("native-app");

createRoot(root).render(
  <StrictMode>
    {new URLSearchParams(window.location.search).get("view") === "settings" ? <SettingsWindow /> : <App />}
  </StrictMode>,
);
