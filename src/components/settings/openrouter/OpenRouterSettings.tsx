import React from "react";
import { RefreshCcw } from "lucide-react";
import { useTranslation } from "react-i18next";

import { Alert } from "@/components/ui/Alert";
import { SettingContainer, SettingsGroup } from "@/components/ui";
import { Button } from "@/components/ui/Button";
import { ResetButton } from "@/components/ui/ResetButton";
import { useSettings } from "@/hooks/useSettings";

import { ApiKeyField } from "../PostProcessingSettingsApi/ApiKeyField";
import { ModelSelect } from "../PostProcessingSettingsApi/ModelSelect";

const PROVIDER_ID = "openrouter";
const DEFAULT_MODEL = "deepseek/deepseek-v4-flash-0731";

export const OpenRouterSettings: React.FC = () => {
  const { t } = useTranslation();
  const {
    settings,
    postProcessModelOptions,
    updatePostProcessModel,
    updatePostProcessApiKey,
    fetchPostProcessModels,
    isUpdating,
  } = useSettings();

  const model =
    settings?.post_process_models?.[PROVIDER_ID]?.trim() || DEFAULT_MODEL;
  const apiKey = settings?.post_process_api_keys?.[PROVIDER_ID] ?? "";
  const modelOptions = (postProcessModelOptions[PROVIDER_ID] ?? []).map(
    (value) => ({
      value,
      label: value,
    }),
  );

  const isModelUpdating = isUpdating(`post_process_model:${PROVIDER_ID}`);
  const isFetchingModels = isUpdating(`post_process_models_fetch:${PROVIDER_ID}`);
  const isApiKeyUpdating = isUpdating(`post_process_api_key:${PROVIDER_ID}`);

  return (
    <>
      <SettingsGroup title={t("settings.openrouter.title")}>
        <SettingContainer
          title={t("settings.openrouter.endpoint.title")}
          description={t("settings.openrouter.endpoint.description")}
          descriptionMode="tooltip"
          layout="stacked"
          grouped={true}
        >
          <Alert variant="info" className="rounded-lg">
            {t("settings.openrouter.endpoint.info")}
          </Alert>
        </SettingContainer>

        <SettingContainer
          title={t("settings.openrouter.apiKey.title")}
          description={t("settings.openrouter.apiKey.description")}
          descriptionMode="tooltip"
          layout="stacked"
          grouped={true}
        >
          <ApiKeyField
            value={apiKey}
            onBlur={(value) => void updatePostProcessApiKey(PROVIDER_ID, value)}
            placeholder={t("settings.openrouter.apiKey.placeholder")}
            disabled={isApiKeyUpdating}
          />
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
