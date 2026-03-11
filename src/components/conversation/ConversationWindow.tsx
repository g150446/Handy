import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Bot, LoaderCircle, User } from "lucide-react";
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
    return <Bot className="h-3 w-3" />;
  }

  return <User className="h-3 w-3" />;
};

export const ConversationWindow = () => {
  const { t, i18n } = useTranslation();
  const direction = getLanguageDirection(i18n.language);
  const { settings } = useSettings();
  const [mode, setMode] = useState<ConversationModeSnapshot | null>(null);
  const lastSessionIdRef = useRef<number>(0);
  const prevMessageCountRef = useRef<number>(0);
  const lastAssistantRef = useRef<HTMLDivElement | null>(null);

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

  // Scroll to start of new assistant message when it arrives
  useEffect(() => {
    const prevCount = prevMessageCountRef.current;
    const currentCount = messages.length;

    if (currentCount > prevCount) {
      const lastMsg = messages[currentCount - 1];
      if (lastMsg?.role === "assistant" && lastAssistantRef.current) {
        lastAssistantRef.current.scrollIntoView({ behavior: "smooth", block: "start" });
      }
    }

    prevMessageCountRef.current = currentCount;
  }, [messages]);

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

  // Index of last assistant message for ref assignment
  const lastAssistantIndex = messages.reduce(
    (last, msg, i) => (msg.role === "assistant" ? i : last),
    -1,
  );

  return (
    <div
      dir={direction}
      className="h-screen flex flex-col bg-background text-text"
    >
      <div className="border-b border-mid-gray/20 px-3 py-2 flex items-center justify-between gap-2">
        <h1 className="text-sm font-semibold truncate">
          {t("conversation.title")}
        </h1>
        <div className="shrink-0 rounded-full bg-mid-gray/10 px-2 py-0.5 text-xs font-medium">
          {statusText}
        </div>
      </div>

      {mode?.active && mode.api_key_source === "missing" && (
        <Alert variant="warning" className="mx-3 mt-2 rounded-lg text-xs py-2">
          {t("conversation.errors.missingApiKey")}
        </Alert>
      )}

      {mode?.active && mode.api_key_source !== "missing" && !model.trim() && (
        <Alert variant="warning" className="mx-3 mt-2 rounded-lg text-xs py-2">
          {t("conversation.errors.missingModel")}
        </Alert>
      )}

      <div className="flex-1 overflow-y-auto px-3 py-3">
        {messages.length === 0 ? (
          <div className="h-full flex items-center justify-center">
            <p className="text-xs text-mid-gray text-center">
              {t("conversation.empty.title")}
            </p>
          </div>
        ) : (
          <div className="flex flex-col gap-2">
            {messages.map((message, index) => {
              const isAssistant = message.role === "assistant";
              const isLastAssistant = index === lastAssistantIndex;

              return (
                <div
                  key={`${message.role}-${index}`}
                  ref={isLastAssistant ? lastAssistantRef : undefined}
                  className={`flex gap-2 ${
                    isAssistant ? "justify-start" : "justify-end"
                  }`}
                >
                  <div
                    className={`max-w-[90%] rounded-xl border px-3 py-2 ${
                      isAssistant
                        ? "border-mid-gray/20 bg-mid-gray/10"
                        : "border-logo-primary/20 bg-logo-primary/15"
                    }`}
                  >
                    <div className="mb-1 flex items-center gap-1 text-xs font-semibold uppercase tracking-wide text-mid-gray">
                      {roleIcon(message.role)}
                      <span>
                        {isAssistant
                          ? t("conversation.roles.assistant")
                          : t("conversation.roles.user")}
                      </span>
                    </div>
                    <p className="whitespace-pre-wrap break-words text-xs leading-5">
                      {message.content}
                    </p>
                  </div>
                </div>
              );
            })}

            {isSending && (
              <div className="flex justify-start">
                <div className="rounded-xl border border-mid-gray/20 bg-mid-gray/10 px-3 py-2 text-xs text-mid-gray">
                  <span className="inline-flex items-center gap-1.5">
                    <LoaderCircle className="h-3 w-3 animate-spin" />
                    {t("conversation.sending")}
                  </span>
                </div>
              </div>
            )}
          </div>
        )}
      </div>

      {error && (
        <div className="px-3 pb-2">
          <Alert variant="error" className="rounded-lg text-xs py-2">
            {error}
          </Alert>
        </div>
      )}
    </div>
  );
};
