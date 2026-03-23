import React from 'react';
import ReactDOM from 'react-dom/client';
import { BrowserRouter } from 'react-router-dom';
import App from './App';
import { basePath } from './lib/basePath';
import './index.css';

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    {/* basePath is injected by the Rust gateway at serve time for reverse-proxy prefix support. */}
    <BrowserRouter basename={basePath || '/'}>
      <App />
    </BrowserRouter>
  </React.StrictMode>
);
