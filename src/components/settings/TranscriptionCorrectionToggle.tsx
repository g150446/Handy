import React from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { useSettings } from "../../hooks/useSettings";

interface TranscriptionCorrectionToggleProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const TranscriptionCorrectionToggle: React.FC<TranscriptionCorrectionToggleProps> =
  React.memo(({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();

    const enabled = getSetting("transcription_correction_enabled") || false;

    return (
      <ToggleSwitch
        checked={enabled}
        onChange={(value) =>
          updateSetting("transcription_correction_enabled", value)
        }
        isUpdating={isUpdating("transcription_correction_enabled")}
        label={t("settings.transcriptionCorrection.label")}
        description={t("settings.transcriptionCorrection.description")}
        descriptionMode={descriptionMode}
        grouped={grouped}
      />
    );
  });
