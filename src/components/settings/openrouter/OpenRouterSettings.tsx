import React from "react";
import { RefreshCcw } from "lucide-react";
import { useTranslation } from "react-i18next";

import { Alert } from "@/components/ui/Alert";
import { SettingContainer, SettingsGroup } from "@/components/ui";
import { Button } from "@/components/ui/Button";
import { ResetButton } from "@/components/ui/ResetButton";
import { useSettings } from "@/hooks/useSettings";

import { ModelSelect } from "../PostProcessingSettingsApi/ModelSelect";

const PROVIDER_ID = "groq";
const GROQ_API_KEY_ENV_VAR = "GROQ_API_KEY";

export const OpenRouterSettings: React.FC = () => {
  const { t } = useTranslation();
  const {
    settings,
    postProcessModelOptions,
    updatePostProcessModel,
    fetchPostProcessModels,
    isUpdating,
  } = useSettings();

  const model = settings?.post_process_models?.[PROVIDER_ID] ?? "";
  const modelOptions = (postProcessModelOptions[PROVIDER_ID] ?? []).map(
    (value) => ({
      value,
      label: value,
    }),
  );

  const isModelUpdating = isUpdating(`post_process_model:${PROVIDER_ID}`);
  const isFetchingModels = isUpdating(`post_process_models_fetch:${PROVIDER_ID}`);

  return (
    <>
      <SettingsGroup title={t("settings.openrouter.title")}>
        <SettingContainer
          title={t("settings.openrouter.apiKey.title")}
          description={t("settings.openrouter.apiKey.description")}
          descriptionMode="tooltip"
          layout="stacked"
          grouped={true}
        >
          <Alert variant="info" className="rounded-lg">
            {t("settings.openrouter.apiKey.environment", {
              envVar: GROQ_API_KEY_ENV_VAR,
            })}
          </Alert>
        </SettingContainer>

        <SettingContainer
          title={t("settings.openrouter.model.title")}
          description={t("settings.openrouter.model.description")}
          descriptionMode="tooltip"
          layout="stacked"
          grouped={true}
        >
          <div className="flex items-center gap-2">
            <ModelSelect
              value={model}
              options={modelOptions}
              disabled={isModelUpdating}
              isLoading={isFetchingModels}
              placeholder={t("settings.openrouter.model.placeholder")}
              onSelect={(value) =>
                void updatePostProcessModel(PROVIDER_ID, value)
              }
              onCreate={(value) =>
                void updatePostProcessModel(PROVIDER_ID, value)
              }
              onBlur={() => {}}
              className="flex-1 min-w-[380px]"
            />
            <ResetButton
              onClick={() => void fetchPostProcessModels(PROVIDER_ID)}
              disabled={isFetchingModels}
              ariaLabel={t("settings.openrouter.model.refreshModels")}
              className="flex h-10 w-10 items-center justify-center"
            >
              <RefreshCcw
                className={`h-4 w-4 ${isFetchingModels ? "animate-spin" : ""}`}
              />
            </ResetButton>
          </div>
        </SettingContainer>
      </SettingsGroup>

      <SettingsGroup title={t("settings.openrouter.conversation.title")}>
        <Alert variant="info" className="rounded-lg">
          {t("settings.openrouter.conversation.description")}
        </Alert>
        <div className="px-1">
          <Button
            variant="secondary"
            onClick={() => void fetchPostProcessModels(PROVIDER_ID)}
            disabled={isFetchingModels}
          >
            {t("settings.openrouter.conversation.refreshAction")}
          </Button>
        </div>
      </SettingsGroup>
    </>
  );
};
