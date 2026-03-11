import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./app/App";
import { createAppGateway } from "./remote/appGateway";
import "./styles.css";

const container = document.getElementById("root");

if (!container) {
  throw new Error("Failed to find the root container.");
}

const gateway = await createAppGateway();

createRoot(container).render(
  <StrictMode>
    <App gateway={gateway} />
  </StrictMode>,
);
