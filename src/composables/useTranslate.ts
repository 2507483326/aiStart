import { ref } from "vue";

import { translateApi } from "@/lib/ipc";
import { toErrorMessage } from "@/lib/notify";

/** 单块内容的翻译状态：首次调用翻译，之后在「原文 / 译文」之间切换。 */
export function useTranslate() {
  const translated = ref<string | null>(null);
  const translating = ref(false);
  const showing = ref(false);
  const error = ref<string | null>(null);

  async function run(text: string): Promise<void> {
    if (translating.value || !text.trim()) return;
    translating.value = true;
    error.value = null;
    try {
      translated.value = await translateApi.text(text);
      showing.value = true;
    } catch (cause) {
      error.value = toErrorMessage(cause, "翻译失败");
    } finally {
      translating.value = false;
    }
  }

  function toggle(text: string): void {
    if (translated.value) {
      showing.value = !showing.value;
      return;
    }
    void run(text);
  }

  function reset(): void {
    translated.value = null;
    translating.value = false;
    showing.value = false;
    error.value = null;
  }

  return { translated, translating, showing, error, run, toggle, reset };
}
