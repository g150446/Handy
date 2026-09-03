import React, { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { toast } from "sonner";

import { Button } from "../ui/Button";
import { Input } from "../ui/Input";
import { SettingContainer } from "../ui/SettingContainer";

type HarborPairStatus = {
  paired: boolean;
  server_id: string | null;
  base_url: string | null;
};

export const HarborPairing: React.FC<{
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}> = ({ descriptionMode = "tooltip", grouped = false }) => {
  const { t } = useTranslation();
  const [status, setStatus] = useState<HarborPairStatus | null>(null);
  const [pairUri, setPairUri] = useState("");
  const [busy, setBusy] = useState(false);
  const [showManual, setShowManual] = useState(false);

  const refresh = useCallback(async () => {
    try {
      const next = await invoke<HarborPairStatus>("get_terminal_harbor_pairing");
      setStatus(next);
    } catch (error) {
      console.error("Failed to load Terminal Harbor pairing:", error);
    }
  }, []);

  useEffect(() => {
    void refresh();
    void (async () => {
      try {
        const next = await invoke<HarborPairStatus>(
          "ensure_terminal_harbor_local_pairing",
        );
        setStatus(next);
      } catch {
        // Harbor may be stopped; leave unpaired until it is running.
      }
    })();
  }, [refresh]);

  const onAutoPair = async () => {
    setBusy(true);
    try {
      const next = await invoke<HarborPairStatus>(
        "ensure_terminal_harbor_local_pairing",
      );
      setStatus(next);
      toast.success(t("settings.general.harbor.pairedToast"));
    } catch (error) {
      console.error(error);
      toast.error(
        t("settings.general.harbor.errors.pairFailed", {
          error: String(error),
        }),
      );
    } finally {
      setBusy(false);
      void refresh();
    }
  };

  const onPair = async () => {
    const uri = pairUri.trim();
    if (!uri) {
      toast.error(t("settings.general.harbor.errors.emptyUri"));
      return;
    }
    setBusy(true);
    try {
      const next = await invoke<HarborPairStatus>("pair_terminal_harbor", {
        pairUri: uri,
      });
      setStatus(next);
      setPairUri("");
      toast.success(t("settings.general.harbor.pairedToast"));
    } catch (error) {
      console.error(error);
      toast.error(
        t("settings.general.harbor.errors.pairFailed", {
          error: String(error),
        }),
      );
    } finally {
      setBusy(false);
      void refresh();
    }
  };

  const statusLabel = status?.paired
    ? t("settings.general.harbor.paired", {
        server: status.server_id?.slice(0, 8) ?? "",
      })
    : t("settings.general.harbor.unpaired");

  return (
    <SettingContainer
      title={t("settings.general.harbor.title")}
      description={t("settings.general.harbor.description")}
      descriptionMode={descriptionMode}
      grouped={grouped}
      layout="stacked"
    >
      <div className="space-y-2 w-full">
        <p className="text-xs text-mid-gray">{statusLabel}</p>
        {status?.base_url && (
          <p className="text-[11px] text-mid-gray/80 break-all">
            {status.base_url}
          </p>
        )}
        <div className="flex gap-2 items-center flex-wrap">
          <Button
            type="button"
            size="sm"
            variant="primary-soft"
            disabled={busy}
            onClick={() => void onAutoPair()}
          >
            {busy
              ? t("settings.general.harbor.pairing")
              : t("settings.general.harbor.autoPair")}
          </Button>
          <Button
            type="button"
            size="sm"
            variant="ghost"
            disabled={busy}
            onClick={() => setShowManual((value) => !value)}
          >
            {t("settings.general.harbor.manualToggle")}
          </Button>
        </div>
        {showManual && (
          <div className="flex gap-2 items-center">
            <Input
              className="flex-1 min-w-0"
              value={pairUri}
              onChange={(event) => setPairUri(event.target.value)}
              placeholder={t("settings.general.harbor.placeholder")}
              disabled={busy}
            />
            <Button
              type="button"
              size="sm"
              variant="secondary"
              disabled={busy}
              onClick={() => void onPair()}
            >
              {t("settings.general.harbor.pair")}
            </Button>
          </div>
        )}
      </div>
    </SettingContainer>
  );
};
