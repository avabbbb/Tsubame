import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import MiniSubtitle from "./MiniSubtitle";
import { isMiniView } from "./miniWindow";
import "./styles/tokens.css";

const root = document.getElementById("root");

if (!root) {
  throw new Error("YUZUKI root element was not found");
}

createRoot(root).render(
  <StrictMode>
    {isMiniView() ? <MiniSubtitle /> : <App />}
  </StrictMode>,
);
