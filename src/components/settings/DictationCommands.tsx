import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import type { SpokenCommand } from "@/bindings";
import { commands as tauriCommands } from "@/bindings";
import { useSettings } from "../../hooks/useSettings";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { Input } from "../ui/Input";
import { Button } from "../ui/Button";

interface DictationCommandsProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

// Turn typed escape sequences into real characters so users can add e.g.
// a "\n" replacement from a plain text input.
const unescape = (s: string) => s.replace(/\\n/g, "\n").replace(/\\t/g, "\t");

// Friendly, non-literal display for whitespace replacements.
const displayReplacement = (r: string): string => {
  if (r === "\n") return "↵";
  if (r === "\n\n") return "¶";
  if (r === "\t") return "⇥";
  return r;
};

export const DictationCommands: React.FC<DictationCommandsProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();

    const enabled = getSetting("spoken_commands_enabled") ?? true;
    const list = (getSetting("spoken_commands") ?? []) as SpokenCommand[];
    const busy = isUpdating("spoken_commands");

    const [newPhrase, setNewPhrase] = useState("");
    const [newReplacement, setNewReplacement] = useState("");

    const persist = (next: SpokenCommand[]) =>
      updateSetting("spoken_commands", next);

    const toggleAt = (index: number) =>
      persist(
        list.map((c, i) =>
          i === index ? { ...c, enabled: !c.enabled } : c,
        ),
      );

    const removeAt = (index: number) =>
      persist(list.filter((_, i) => i !== index));

    const addCommand = () => {
      const phrase = newPhrase.trim().toLowerCase();
      const replacement = unescape(newReplacement);
      if (!phrase || !replacement) return;
      if (list.some((c) => c.phrase.toLowerCase() === phrase)) {
        toast.error(
          t("settings.dictationCommands.duplicate", { phrase }),
        );
        return;
      }
      persist([{ phrase, replacement, enabled: true }, ...list]);
      setNewPhrase("");
      setNewReplacement("");
    };

    const resetDefaults = async () => {
      try {
        const res = await tauriCommands.resetSpokenCommands();
        if (res.status === "ok") {
          // Reflect the restored defaults in the store without a second write.
          persist(res.data);
        }
      } catch (e) {
        toast.error(t("settings.dictationCommands.resetFailed"));
        console.error("resetSpokenCommands failed:", e);
      }
    };

    return (
      <>
        <ToggleSwitch
          checked={enabled}
          onChange={(v) => updateSetting("spoken_commands_enabled", v)}
          isUpdating={isUpdating("spoken_commands_enabled")}
          label={t("settings.dictationCommands.title")}
          description={t("settings.dictationCommands.description")}
          descriptionMode={descriptionMode}
          grouped={grouped}
        />

        {enabled && (
          <div
            className={`px-4 p-3 space-y-3 ${grouped ? "" : "rounded-lg border border-mid-gray/20"}`}
          >
            {/* Add a new command */}
            <div className="flex items-center gap-2">
              <Input
                type="text"
                className="flex-1"
                value={newPhrase}
                onChange={(e) => setNewPhrase(e.target.value)}
                placeholder={t("settings.dictationCommands.phrasePlaceholder")}
                variant="compact"
                disabled={busy}
              />
              <span className="text-mid-gray">→</span>
              <Input
                type="text"
                className="max-w-24"
                value={newReplacement}
                onChange={(e) => setNewReplacement(e.target.value)}
                placeholder={t(
                  "settings.dictationCommands.replacementPlaceholder",
                )}
                variant="compact"
                disabled={busy}
              />
              <Button
                onClick={addCommand}
                disabled={!newPhrase.trim() || !newReplacement || busy}
                variant="primary"
                size="md"
              >
                {t("settings.dictationCommands.add")}
              </Button>
            </div>

            {/* Existing commands */}
            <div className="max-h-64 overflow-y-auto flex flex-col gap-1">
              {list.map((cmd, index) => (
                <div
                  key={`${cmd.phrase}-${index}`}
                  className="flex items-center gap-2 text-sm py-1 border-b border-mid-gray/10 last:border-0"
                >
                  <input
                    type="checkbox"
                    checked={cmd.enabled}
                    onChange={() => toggleAt(index)}
                    disabled={busy}
                    aria-label={t("settings.dictationCommands.toggle", {
                      phrase: cmd.phrase,
                    })}
                    className="cursor-pointer"
                  />
                  <span
                    className={`flex-1 ${cmd.enabled ? "" : "line-through opacity-50"}`}
                  >
                    {cmd.phrase}
                  </span>
                  <span className="text-mid-gray">→</span>
                  <span className="min-w-8 font-mono">
                    {displayReplacement(cmd.replacement)}
                  </span>
                  <button
                    onClick={() => removeAt(index)}
                    disabled={busy}
                    aria-label={t("settings.dictationCommands.remove", {
                      phrase: cmd.phrase,
                    })}
                    className="text-mid-gray hover:text-red-500 cursor-pointer px-1"
                  >
                    <svg
                      className="w-3.5 h-3.5"
                      fill="none"
                      stroke="currentColor"
                      viewBox="0 0 24 24"
                    >
                      <path
                        strokeLinecap="round"
                        strokeLinejoin="round"
                        strokeWidth={2}
                        d="M6 18L18 6M6 6l12 12"
                      />
                    </svg>
                  </button>
                </div>
              ))}
            </div>

            <div className="flex justify-end">
              <Button
                onClick={resetDefaults}
                disabled={busy}
                variant="secondary"
                size="sm"
              >
                {t("settings.dictationCommands.reset")}
              </Button>
            </div>
          </div>
        )}
      </>
    );
  },
);
