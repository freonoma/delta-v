import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import { SettingsWindow } from "./SettingsWindow";
import "./styles.css";
import "./SettingsWindow.css";

const root = document.getElementById("root");

if (!root) {
  throw new Error("The app root is missing.");
}

createRoot(root).render(
  <StrictMode>
    {new URLSearchParams(window.location.search).get("view") === "settings" ? <SettingsWindow /> : <App />}
  </StrictMode>,
);
