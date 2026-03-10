import React from 'react';
import ReactDOM from 'react-dom/client';
import { BrowserRouter } from 'react-router-dom';
import App from './App';
import './index.css';

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    {/* Match React Router paths to Vite's public base so in-app links resolve under /_app/. */}
    <BrowserRouter basename="/_app">
      <App />
    </BrowserRouter>
  </React.StrictMode>
);
