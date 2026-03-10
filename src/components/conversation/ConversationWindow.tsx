import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Bot, LoaderCircle, MessageSquare, Sparkles, User } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { Alert } from "@/components/ui/Alert";
import { useSettings } from "@/hooks/useSettings";
import { getLanguageDirection } from "@/lib/utils/rtl";

type ConversationTurn = {
  role: string;
  content: string;
};

type ConversationModeSnapshot = {
  active: boolean;
  session_id: number;
  messages: ConversationTurn[];
  is_sending: boolean;
  last_error: string | null;
  api_key_source: "missing" | "settings" | "environment";
};

const OPENROUTER_PROVIDER_ID = "openrouter";

const roleIcon = (role: string) => {
  if (role === "assistant") {
    return <Bot className="h-4 w-4" />;
  }

  return <User className="h-4 w-4" />;
};

export const ConversationWindow = () => {
  const { t, i18n } = useTranslation();
  const direction = getLanguageDirection(i18n.language);
  const { settings } = useSettings();
  const [mode, setMode] = useState<ConversationModeSnapshot | null>(null);
  const lastSessionIdRef = useRef<number>(0);

  const model = settings?.post_process_models?.[OPENROUTER_PROVIDER_ID] ?? "";
  const messages = mode?.messages ?? [];
  const isSending = mode?.is_sending ?? false;
  const error = mode?.last_error ?? null;

  const refreshMode = async () => {
    try {
      const snapshot = await invoke<ConversationModeSnapshot>("get_conversation_mode");
      lastSessionIdRef.current = snapshot.session_id;
      setMode(snapshot);
    } catch (invokeError) {
      console.error("Failed to refresh conversation mode:", invokeError);
    }
  };

  useEffect(() => {
    const loadMode = async () => {
      await refreshMode();
    };

    loadMode();

    const setupListener = async () => {
      const unlisten = await listen<ConversationModeSnapshot>(
        "conversation-mode-changed",
        (event) => {
          const snapshot = event.payload;
          setMode(snapshot);

          const sessionChanged = snapshot.session_id !== lastSessionIdRef.current;
          lastSessionIdRef.current = snapshot.session_id;
          void sessionChanged;
        },
      );

      return unlisten;
    };

    let cleanup: (() => void) | undefined;
    setupListener().then((unlisten) => {
      cleanup = unlisten;
    });

    return () => {
      cleanup?.();
    };
  }, []);

  useEffect(() => {
    void refreshMode();

    const intervalId = window.setInterval(() => {
      void refreshMode();
    }, mode?.active ? 1000 : 3000);

    const handleFocus = () => {
      void refreshMode();
    };

    window.addEventListener("focus", handleFocus);

    return () => {
      window.clearInterval(intervalId);
      window.removeEventListener("focus", handleFocus);
    };
  }, [mode?.active]);

  const statusText = useMemo(() => {
    if (!mode?.active) {
      return t("conversation.status.inactive");
    }

    if (mode.api_key_source === "missing") {
      return t("conversation.status.missingApiKey");
    }

    if (!model.trim()) {
      return t("conversation.status.missingModel");
    }

    return t("conversation.status.active", { model });
  }, [mode?.active, mode?.api_key_source, model, t]);

  return (
    <div
      dir={direction}
      className="h-screen flex flex-col bg-background text-text"
    >
      <div className="border-b border-mid-gray/20 px-5 py-4 flex items-center justify-between gap-4">
        <div className="flex items-center gap-3">
          <div className="rounded-full bg-logo-primary/20 p-2">
            <Sparkles className="h-5 w-5 text-logo-stroke" />
          </div>
          <div>
            <h1 className="text-lg font-semibold">
              {t("conversation.title")}
            </h1>
            <p className="text-sm text-mid-gray">
              {t("conversation.subtitle")}
            </p>
          </div>
        </div>
        <div className="rounded-full bg-mid-gray/10 px-3 py-1 text-xs font-medium">
          {statusText}
        </div>
      </div>

      {!mode?.active && (
        <Alert variant="info" className="m-4 rounded-lg">
          {t("conversation.help.toggleBack")}
        </Alert>
      )}

      {mode?.active && mode.api_key_source === "missing" && (
        <Alert variant="warning" className="m-4 rounded-lg">
          {t("conversation.errors.missingApiKey")}
        </Alert>
      )}

      {mode?.active && mode.api_key_source !== "missing" && !model.trim() && (
        <Alert variant="warning" className="m-4 rounded-lg">
          {t("conversation.errors.missingModel")}
        </Alert>
      )}

      <div className="flex-1 overflow-y-auto px-4 py-4">
        {messages.length === 0 ? (
          <div className="h-full flex items-center justify-center">
            <div className="max-w-md rounded-2xl border border-mid-gray/20 bg-mid-gray/5 p-6 text-center">
              <MessageSquare className="mx-auto mb-3 h-10 w-10 text-logo-stroke" />
              <h2 className="mb-2 text-base font-semibold">
                {t("conversation.empty.title")}
              </h2>
              <p className="text-sm text-mid-gray">
                {t("conversation.empty.description")}
              </p>
            </div>
          </div>
        ) : (
          <div className="mx-auto flex max-w-3xl flex-col gap-3">
            {messages.map((message, index) => {
              const isAssistant = message.role === "assistant";

              return (
                <div
                  key={`${message.role}-${index}`}
                  className={`flex gap-3 ${
                    isAssistant ? "justify-start" : "justify-end"
                  }`}
                >
                  <div
                    className={`max-w-[85%] rounded-2xl border px-4 py-3 ${
                      isAssistant
                        ? "border-mid-gray/20 bg-mid-gray/10"
                        : "border-logo-primary/20 bg-logo-primary/15"
                    }`}
                  >
                    <div className="mb-2 flex items-center gap-2 text-xs font-semibold uppercase tracking-wide text-mid-gray">
                      {roleIcon(message.role)}
                      <span>
                        {isAssistant
                          ? t("conversation.roles.assistant")
                          : t("conversation.roles.user")}
                      </span>
                    </div>
                    <p className="whitespace-pre-wrap break-words text-sm leading-6">
                      {message.content}
                    </p>
                  </div>
                </div>
              );
            })}

            {isSending && (
              <div className="flex justify-start">
                <div className="rounded-2xl border border-mid-gray/20 bg-mid-gray/10 px-4 py-3 text-sm text-mid-gray">
                  <span className="inline-flex items-center gap-2">
                    <LoaderCircle className="h-4 w-4 animate-spin" />
                    {t("conversation.sending")}
                  </span>
                </div>
              </div>
            )}
          </div>
        )}
      </div>

      <div className="border-t border-mid-gray/20 px-4 py-3">
        {error && (
          <Alert variant="error" className="mb-3 rounded-lg">
            {error}
          </Alert>
        )}

        <div className="mx-auto max-w-3xl">
          <p className="text-xs text-mid-gray">
            {t("conversation.help.toggleBack")}
          </p>
        </div>
      </div>
    </div>
  );
};
