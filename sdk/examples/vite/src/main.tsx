import { createRoot } from "react-dom/client";
import { AppProvider } from "./AppProvider";

import "@leftcurve/ui/fonts/ABCDiatypeRounded/index.css";
import "./index.css";

import App from "./App";

createRoot(document.getElementById("root")!).render(
  <AppProvider>
    <App />
  </AppProvider>,
);
