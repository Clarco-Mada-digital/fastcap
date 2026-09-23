import React from "react";
import ReactDOM from "react-dom/client";
import { getCurrentWindow } from "@tauri-apps/api/window";
import App from "./App";
import { RegionSelector } from "./components/RegionSelector";
import { RecIndicator } from "./components/RecIndicator";
import "./styles/global.css";

// La même interface sert plusieurs fenêtres Tauri :
//  - "main"           : application complète
//  - "region-overlay" : sélection d'une zone sur une image figée
//  - "rec-indicator"  : indicateur "REC" flottant pendant l'enregistrement
const isRegionOverlay = getCurrentWindow().label === "region-overlay";
const isRecIndicator = getCurrentWindow().label === "rec-indicator";
const Root = isRegionOverlay ? RegionSelector : isRecIndicator ? RecIndicator : App;

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <Root />
  </React.StrictMode>,
);
