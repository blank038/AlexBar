import React from 'react';
import ReactDOM from 'react-dom/client';
import { getCurrentWebviewWindow } from '@tauri-apps/api/webviewWindow';
import App from './App';
import SettingsApp from './SettingsApp';
import './styles/global.css';


const CurrentApp = getCurrentWebviewWindow().label === 'settings' ? SettingsApp : App;
ReactDOM.createRoot(document.getElementById('root') as HTMLElement).render(
  <React.StrictMode>
    <CurrentApp />
  </React.StrictMode>,
);
