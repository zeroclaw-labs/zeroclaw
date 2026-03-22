import { useState, useCallback, useEffect } from "react";
import { t, type Locale } from "../lib/i18n";
import { apiClient, type DeviceInfo } from "../lib/api";

interface DeviceSelectProps {
  locale: Locale;
  devices: DeviceInfo[];
  onDeviceSelected: () => void;
  onLogout: () => void;
}

export function DeviceSelect({ locale, devices, onDeviceSelected, onLogout }: DeviceSelectProps) {
  const [deviceList, setDeviceList] = useState<DeviceInfo[]>(devices);
  const [selectedDevice, setSelectedDevice] = useState<DeviceInfo | null>(null);
  const [pairingCode, setPairingCode] = useState("");
  const [isVerifying, setIsVerifying] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [autoConnecting, setAutoConnecting] = useState(false);

  const currentDeviceId = apiClient.getDeviceId();

  // Auto-connect logic on mount
  useEffect(() => {
    const onlineDevices = deviceList.filter((d) => d.is_online);

    if (deviceList.length === 0) {
      // No devices registered → auto-register this device and proceed
      setAutoConnecting(true);
      apiClient.registerCurrentDevice().then(() => {
        apiClient.startHeartbeat();
        onDeviceSelected();
      }).catch(() => {
        // Registration failed, still proceed (server will auto-register via heartbeat)
        apiClient.startHeartbeat();
        onDeviceSelected();
      });
      return;
    }

    if (deviceList.length === 1) {
      const device = deviceList[0];
      const isLocal = device.device_id === currentDeviceId;
      if (isLocal || !device.has_pairing_code) {
        // Only 1 device and it's local or no pairing code → auto-connect
        setAutoConnecting(true);
        apiClient.startHeartbeat();
        onDeviceSelected();
        return;
      }
    }

    // Multiple devices but only 1 online
    if (onlineDevices.length === 1) {
      const device = onlineDevices[0];
      const isLocal = device.device_id === currentDeviceId;
      if (isLocal || !device.has_pairing_code) {
        setAutoConnecting(true);
        apiClient.startHeartbeat();
        onDeviceSelected();
        return;
      }
    }

    // Check if current device is in the list
    const currentInList = deviceList.find((d) => d.device_id === currentDeviceId);
    if (currentInList) {
      // This device exists → auto-select if it's the only option
      // (the auto-connect cases above already handle single device)
      // For multiple devices, show the list
    }
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  const handleSelectDevice = useCallback((device: DeviceInfo) => {
    setSelectedDevice(device);
    setError(null);
    setPairingCode("");

    const isLocal = device.device_id === currentDeviceId;
    if (isLocal || !device.has_pairing_code) {
      // Local device or no pairing code → connect directly
      apiClient.startHeartbeat();
      onDeviceSelected();
    }
    // Remote device with pairing code → show pairing input
  }, [currentDeviceId, onDeviceSelected]);

  const handleVerifyPairing = useCallback(async () => {
    if (!selectedDevice || !pairingCode.trim()) return;

    setIsVerifying(true);
    setError(null);

    try {
      const verified = await apiClient.verifyDevicePairing(selectedDevice.device_id, pairingCode.trim());
      if (verified) {
        apiClient.startHeartbeat();
        onDeviceSelected();
      } else {
        setError(t("pairing_invalid", locale));
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : t("pairing_invalid", locale));
    } finally {
      setIsVerifying(false);
    }
  }, [selectedDevice, pairingCode, locale, onDeviceSelected]);

  const handleRefresh = useCallback(async () => {
    try {
      const updated = await apiClient.getDevices();
      setDeviceList(updated);
    } catch {
      // Ignore refresh errors
    }
  }, []);

  if (autoConnecting) {
    return (
      <div className="auth-container">
        <div className="auth-card">
          <div className="auth-logo">
            <div className="auth-logo-icon">ZC</div>
            <p className="auth-subtitle">{t("auto_connecting", locale)}</p>
          </div>
        </div>
      </div>
    );
  }

  // Show pairing code input for selected remote device
  if (selectedDevice && selectedDevice.device_id !== currentDeviceId && selectedDevice.has_pairing_code) {
    return (
      <div className="auth-container">
        <div className="auth-card">
          <div className="auth-logo">
            <div className="auth-logo-icon">ZC</div>
            <h1 className="auth-title">{t("device_pairing_required", locale)}</h1>
            <p className="auth-subtitle">{selectedDevice.device_name}</p>
          </div>

          {error && <div className="auth-error">{error}</div>}

          <div className="auth-field">
            <label className="auth-label">{t("pairing_code", locale)}</label>
            <input
              className="auth-input"
              type="password"
              value={pairingCode}
              onChange={(e) => setPairingCode(e.target.value)}
              onKeyDown={(e) => { if (e.key === "Enter") handleVerifyPairing(); }}
              placeholder={t("enter_pairing_code", locale)}
              autoFocus
              disabled={isVerifying}
            />
          </div>

          <button
            className="auth-btn auth-btn-primary"
            onClick={handleVerifyPairing}
            disabled={isVerifying || !pairingCode.trim()}
          >
            {isVerifying ? t("verifying", locale) : t("verify_pairing", locale)}
          </button>

          <div className="auth-link">
            <button className="auth-link-btn" onClick={() => { setSelectedDevice(null); setError(null); }}>
              {t("back_to_chat", locale)}
            </button>
          </div>
        </div>
      </div>
    );
  }

  // Device list view
  return (
    <div className="auth-container">
      <div className="auth-card" style={{ maxWidth: 480 }}>
        <div className="auth-logo">
          <div className="auth-logo-icon">ZC</div>
          <h1 className="auth-title">{t("select_device", locale)}</h1>
          <p className="auth-subtitle">{t("select_device_subtitle", locale)}</p>
        </div>

        {error && <div className="auth-error">{error}</div>}

        <div className="device-list">
          {deviceList.length === 0 ? (
            <div className="device-empty">{t("device_no_devices", locale)}</div>
          ) : (
            deviceList.map((device) => {
              const isLocal = device.device_id === currentDeviceId;
              return (
                <button
                  key={device.device_id}
                  className={`device-item ${device.is_online ? "online" : "offline"}`}
                  onClick={() => handleSelectDevice(device)}
                  disabled={!device.is_online && !isLocal}
                >
                  <div className="device-item-info">
                    <div className="device-item-name">
                      {device.device_name}
                      {isLocal && (
                        <span className="device-badge device-badge-local">
                          {t("device_this", locale)}
                        </span>
                      )}
                      {!isLocal && (
                        <span className="device-badge device-badge-remote">
                          {t("device_remote", locale)}
                        </span>
                      )}
                    </div>
                    <div className="device-item-meta">
                      {device.platform && <span>{device.platform}</span>}
                      {device.has_pairing_code && !isLocal && (
                        <span className="device-lock-icon" title={t("device_pairing_required", locale)}>
                          <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                            <rect x="3" y="11" width="18" height="11" rx="2" ry="2" />
                            <path d="M7 11V7a5 5 0 0 1 10 0v4" />
                          </svg>
                        </span>
                      )}
                    </div>
                  </div>
                  <div className={`device-status ${device.is_online ? "online" : "offline"}`}>
                    <div className={`status-dot ${device.is_online ? "connected" : ""}`} />
                    <span>{device.is_online ? t("device_online", locale) : t("device_offline", locale)}</span>
                  </div>
                </button>
              );
            })
          )}
        </div>

        <div className="device-actions">
          <button className="auth-link-btn" onClick={handleRefresh}>
            {locale === "ko" ? "\uC0C8\uB85C\uACE0\uCE68" : "Refresh"}
          </button>
          <button className="auth-link-btn" onClick={onLogout}>
            {t("logout", locale)}
          </button>
        </div>
      </div>
    </div>
  );
}
