import React from "react";
import { useTranslation } from "react-i18next";
import { Dropdown } from "../ui/Dropdown";
import { SettingContainer } from "../ui/SettingContainer";
import { useSettings } from "../../hooks/useSettings";
import type { PreferredControlMode } from "@/bindings";

interface PreferredControlModeProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const PreferredControlModeSetting: React.FC<PreferredControlModeProps> =
  React.memo(({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();

    const selected = (getSetting("preferred_control_mode") ||
      "harbor") as PreferredControlMode;

    const options = [
      {
        value: "harbor",
        label: t("settings.general.preferredControlMode.options.harbor"),
      },
      {
        value: "desktop",
        label: t("settings.general.preferredControlMode.options.desktop"),
      },
    ];

    return (
      <SettingContainer
        title={t("settings.general.preferredControlMode.title")}
        description={t("settings.general.preferredControlMode.description")}
        descriptionMode={descriptionMode}
        grouped={grouped}
      >
        <Dropdown
          options={options}
          selectedValue={selected}
          onSelect={(value) =>
            updateSetting(
              "preferred_control_mode",
              value as PreferredControlMode,
            )
          }
          disabled={isUpdating("preferred_control_mode")}
        />
      </SettingContainer>
    );
  });
