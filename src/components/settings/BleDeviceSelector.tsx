import React, { useState, useEffect, useCallback } from "react";
import { useTranslation } from "react-i18next";
import { commands, type AudioSource, type BleStatus } from "@/bindings";
import { SettingContainer } from "../ui/SettingContainer";

interface BleDeviceSelectorProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const BleDeviceSelector: React.FC<BleDeviceSelectorProps> =
  React.memo(({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();

    const [audioSource, setAudioSource] = useState<AudioSource>("microphone");
    const [bleStatus, setBleStatus] = useState<BleStatus>({
      connected: false,
      device_name: null,
      device_address: null,
    });
    const [scannedDevices, setScannedDevices] = useState<string[]>([]);
    const [selectedDevice, setSelectedDevice] = useState<string>("");
    const [isScanning, setIsScanning] = useState(false);
    const [isConnecting, setIsConnecting] = useState(false);

    // Extract address from "Name (address)" display string
    const parseAddress = (displayStr: string): string => {
      const m = displayStr.match(/\(([^)]+)\)$/);
      return m ? m[1] : displayStr;
    };

    const refreshStatus = useCallback(async () => {
      const [sourceRes, statusRes] = await Promise.all([
        commands.getAudioSource(),
        commands.bleGetStatus(),
      ]);
      setAudioSource(sourceRes);
      setBleStatus(statusRes);
    }, []);

    useEffect(() => {
      refreshStatus();
    }, [refreshStatus]);

    const handleSourceChange = async (source: AudioSource) => {
      const result = await commands.setAudioSource(source);
      if (result.status === "ok") {
        setAudioSource(source);
      }
    };

    const handleScan = async () => {
      setIsScanning(true);
      setScannedDevices([]);
      try {
        const result = await commands.bleScanDevices(5);
        if (result.status === "ok") {
          setScannedDevices(result.data);
          if (result.data.length > 0 && !selectedDevice) {
            setSelectedDevice(result.data[0]);
          }
        }
      } finally {
        setIsScanning(false);
      }
    };

    const handleConnect = async () => {
      if (!selectedDevice) return;
      setIsConnecting(true);
      try {
        const address = parseAddress(selectedDevice);
        const result = await commands.bleConnectByAddress(address);
        if (result.status === "ok") {
          setBleStatus(result.data);
          // Automatically switch to BLE source on successful connect
          await handleSourceChange("ble");
        }
      } finally {
        setIsConnecting(false);
      }
    };

    const handleDisconnect = async () => {
      const result = await commands.bleDisconnect();
      if (result.status === "ok") {
        setBleStatus({ connected: false, device_name: null, device_address: null });
        // Switch back to microphone source
        await handleSourceChange("microphone");
      }
    };

    return (
      <SettingContainer
        title={t("settings.sound.ble.title")}
        description={t("settings.sound.ble.description")}
        descriptionMode={descriptionMode}
        grouped={grouped}
        layout="stacked"
      >
        {/* Audio Source Toggle */}
        <div className="flex gap-2 mb-3">
          <button
            onClick={() => handleSourceChange("microphone")}
            className={`flex-1 py-1.5 px-3 rounded text-sm font-medium transition-colors ${
              audioSource === "microphone"
                ? "bg-logo-primary text-white"
                : "bg-mid-gray/10 text-mid-gray hover:bg-mid-gray/20"
            }`}
          >
            {t("settings.sound.ble.source.microphone")}
          </button>
          <button
            onClick={() => handleSourceChange("ble")}
            className={`flex-1 py-1.5 px-3 rounded text-sm font-medium transition-colors ${
              audioSource === "ble"
                ? "bg-logo-primary text-white"
                : "bg-mid-gray/10 text-mid-gray hover:bg-mid-gray/20"
            }`}
          >
            {t("settings.sound.ble.source.ble")}
          </button>
        </div>

        {/* BLE Controls (shown only when BLE source selected or connected) */}
        {(audioSource === "ble" || bleStatus.connected) && (
          <div className="space-y-2">
            {/* Connection status */}
            <div className="flex items-center gap-2">
              <span
                className={`inline-block w-2 h-2 rounded-full ${
                  bleStatus.connected ? "bg-green-500" : "bg-mid-gray/40"
                }`}
              />
              <span className="text-sm text-mid-gray">
                {bleStatus.connected
                  ? `${t("settings.sound.ble.connected")}: ${bleStatus.device_name ?? ""}`
                  : t("settings.sound.ble.disconnected")}
              </span>
            </div>

            {!bleStatus.connected ? (
              <>
                {/* Device selector + Scan */}
                <div className="flex gap-2">
                  <select
                    value={selectedDevice}
                    onChange={(e) => setSelectedDevice(e.target.value)}
                    className="flex-1 text-sm bg-background border border-mid-gray/30 rounded px-2 py-1.5 text-foreground"
                    disabled={isScanning || scannedDevices.length === 0}
                  >
                    {scannedDevices.length === 0 ? (
                      <option value="">{t("settings.sound.ble.selectDevice")}</option>
                    ) : (
                      scannedDevices.map((d) => (
                        <option key={d} value={d}>
                          {d}
                        </option>
                      ))
                    )}
                  </select>
                  <button
                    onClick={handleScan}
                    disabled={isScanning || isConnecting}
                    className="py-1.5 px-3 rounded text-sm font-medium bg-mid-gray/10 hover:bg-mid-gray/20 disabled:opacity-50 transition-colors whitespace-nowrap"
                  >
                    {isScanning
                      ? t("settings.sound.ble.scanning")
                      : t("settings.sound.ble.scan")}
                  </button>
                </div>

                {/* No devices found message */}
                {!isScanning && scannedDevices.length === 0 && (
                  <p className="text-xs text-mid-gray">
                    {t("settings.sound.ble.noDevices")}
                  </p>
                )}

                {/* Connect button */}
                <button
                  onClick={handleConnect}
                  disabled={!selectedDevice || isConnecting || isScanning}
                  className="w-full py-1.5 px-3 rounded text-sm font-medium bg-logo-primary text-white hover:opacity-90 disabled:opacity-50 transition-opacity"
                >
                  {isConnecting
                    ? t("settings.sound.ble.connecting")
                    : t("settings.sound.ble.connect")}
                </button>
              </>
            ) : (
              /* Disconnect button */
              <button
                onClick={handleDisconnect}
                className="w-full py-1.5 px-3 rounded text-sm font-medium bg-mid-gray/10 hover:bg-mid-gray/20 transition-colors"
              >
                {t("settings.sound.ble.disconnect")}
              </button>
            )}
          </div>
        )}
      </SettingContainer>
    );
  });

BleDeviceSelector.displayName = "BleDeviceSelector";
