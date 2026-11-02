import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Input } from "@/components/ui/Input";
import { useSettings } from "@/hooks/useSettings";
import { useModelStore } from "@/stores/modelStore";

const DEFAULT_OLLAMA_URL = "http://localhost:11434";

/** Where Gemma 4 speech-to-text runs: the local Ollama server's URL. */
export const OllamaUrlSetting: React.FC = () => {
  const { t } = useTranslation();
  const { getSetting, updateSetting } = useSettings();
  const ollamaUrl = getSetting("ollama_base_url") ?? DEFAULT_OLLAMA_URL;
  const [draft, setDraft] = useState(ollamaUrl);

  useEffect(() => {
    setDraft(ollamaUrl);
  }, [ollamaUrl]);

  const commit = async () => {
    const next = draft.trim() || DEFAULT_OLLAMA_URL;
    setDraft(next);
    if (next === ollamaUrl) return;
    await updateSetting("ollama_base_url", next);
    await useModelStore.getState().loadModels();
  };

  return (
    <div className="flex flex-col gap-[6px] ps-[42px] pe-[12px] pt-[4px] pb-[8px]">
      <label className="text-small font-medium text-ink-2">
        {t("settings.models.ollamaUrl.title")}
        <Input
          variant="compact"
          className="ms-[8px] w-[240px] font-normal"
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onBlur={() => void commit()}
          placeholder={DEFAULT_OLLAMA_URL}
        />
      </label>
      <p className="text-small text-ink-3">
        {t("settings.models.ollamaUrl.description")}
      </p>
    </div>
  );
};
