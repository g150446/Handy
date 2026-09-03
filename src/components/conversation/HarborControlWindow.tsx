import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Bot, LoaderCircle, User } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { Alert } from "@/components/ui/Alert";
import { getLanguageDirection } from "@/lib/utils/rtl";

type HarborTurn = {
  role: string;
  content: string;
};

type HarborControlSnapshot = {
  active: boolean;
  session_id: number;
  messages: HarborTurn[];
  is_sending: boolean;
  last_error: string | null;
  paired: boolean;
  status: string;
  directories?: string[];
};

const roleIcon = (role: string) => {
  if (role === "assistant") {
    return <Bot className="h-3 w-3" />;
  }
  return <User className="h-3 w-3" />;
};

export const HarborControlWindow = () => {
  const { t, i18n } = useTranslation();
  const direction = getLanguageDirection(i18n.language);
  const [mode, setMode] = useState<HarborControlSnapshot | null>(null);
  const lastSessionIdRef = useRef<number>(0);
  const prevMessageCountRef = useRef<number>(0);
  const lastAssistantRef = useRef<HTMLDivElement | null>(null);

  const messages = mode?.messages ?? [];
  const isSending = mode?.is_sending ?? false;
  const error = mode?.last_error ?? null;

  const refreshMode = async () => {
    try {
      const snapshot = await invoke<HarborControlSnapshot>("get_harbor_control");
      lastSessionIdRef.current = snapshot.session_id;
      setMode(snapshot);
    } catch (invokeError) {
      console.error("Failed to refresh harbor control:", invokeError);
    }
  };

  useEffect(() => {
    void refreshMode();
    let cleanup: (() => void) | undefined;
    void listen<HarborControlSnapshot>("harbor-control-changed", (event) => {
      setMode(event.payload);
      lastSessionIdRef.current = event.payload.session_id;
    }).then((unlisten) => {
      cleanup = unlisten;
    });
    return () => cleanup?.();
  }, []);

  useEffect(() => {
    const intervalId = window.setInterval(
      () => {
        void refreshMode();
      },
      mode?.active ? 1000 : 3000,
    );
    return () => window.clearInterval(intervalId);
  }, [mode?.active]);

  useEffect(() => {
    const prevCount = prevMessageCountRef.current;
    const currentCount = messages.length;
    if (currentCount > prevCount) {
      const lastMsg = messages[currentCount - 1];
      if (lastMsg?.role === "assistant" && lastAssistantRef.current) {
        lastAssistantRef.current.scrollIntoView({
          behavior: "smooth",
          block: "start",
        });
      }
    }
    prevMessageCountRef.current = currentCount;
  }, [messages]);

  const statusText = useMemo(() => {
    if (!mode?.active) {
      return t("harbor.status.inactive");
    }
    if (!mode.paired) {
      return t("harbor.status.unpaired");
    }
    return mode.status || t("harbor.status.ready");
  }, [mode?.active, mode?.paired, mode?.status, t]);

  const lastAssistantIndex = messages.reduce(
    (last, msg, i) => (msg.role === "assistant" ? i : last),
    -1,
  );

  return (
    <div
      dir={direction}
      className="h-screen flex flex-col bg-background text-text"
    >
      <div className="border-b border-mid-gray/20 px-3 py-2 space-y-1">
        <div className="flex items-center justify-between gap-2">
          <h1 className="text-sm font-semibold truncate">
            {t("harbor.title")}
          </h1>
          <div className="shrink-0 rounded-full bg-mid-gray/10 px-2 py-0.5 text-xs font-medium">
            {statusText}
          </div>
        </div>
        <p className="text-[11px] text-mid-gray leading-4">
          {t("harbor.subtitle")}
        </p>
      </div>

      {mode?.active && !mode.paired && (
        <Alert variant="warning" className="mx-3 mt-2 rounded-lg text-xs py-2">
          {t("harbor.errors.unpaired")}
        </Alert>
      )}

      {mode?.active && mode.paired && (mode.directories?.length ?? 0) > 0 && (
        <div className="mx-3 mt-2 rounded-lg border border-mid-gray/20 bg-mid-gray/5 px-2 py-1.5">
          <p className="text-[10px] uppercase tracking-wide text-mid-gray mb-1">
            {t("harbor.directories")}
          </p>
          <p className="text-[11px] leading-4 text-text/90 break-words">
            {(mode.directories ?? []).slice(0, 12).join(" · ")}
          </p>
        </div>
      )}

      <div className="flex-1 overflow-y-auto px-3 py-3">
        {messages.length === 0 ? (
          <div className="h-full flex items-center justify-center">
            <div className="text-center space-y-1">
              <p className="text-xs text-mid-gray">{t("harbor.empty.title")}</p>
              <p className="text-[11px] text-mid-gray/80">
                {t("harbor.empty.description")}
              </p>
            </div>
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
                          ? t("harbor.roles.assistant")
                          : t("harbor.roles.user")}
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
                    {t("harbor.sending")}
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

      <div className="border-t border-mid-gray/20 px-3 py-2">
        <p className="text-[11px] text-mid-gray leading-4">
          {t("harbor.help.toggleBack")}
        </p>
      </div>
    </div>
  );
};
