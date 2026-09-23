import { openUrl as pluginOpenUrl } from "@tauri-apps/plugin-opener";

import { notifyError } from "@/lib/notify";

export async function openUrl(url: string): Promise<void> {
  if (!url) return;
  try {
    await pluginOpenUrl(url);
  } catch (error) {
    notifyError(error, "打开链接失败");
  }
}
