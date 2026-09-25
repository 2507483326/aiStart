import { openUrl as pluginOpenUrl } from "@tauri-apps/plugin-opener";

import { systemApi } from "@/lib/ipc";
import { notifyError } from "@/lib/notify";

export async function openUrl(url: string): Promise<void> {
  if (!url) return;
  try {
    await pluginOpenUrl(url);
  } catch (error) {
    notifyError(error, "打开链接失败");
  }
}

/// 在资源管理器里打开安装包下载文件夹（目录不存在时后端会先建出来）。
export async function openDownloadDir(): Promise<void> {
  try {
    await systemApi.openDownloadDir();
  } catch (error) {
    notifyError(error, "打开下载文件夹失败");
  }
}

/// 在资源管理器里打开应用数据目录（配置与数据库都在这里）。
export async function openDataDir(): Promise<void> {
  try {
    await systemApi.openDataDir();
  } catch (error) {
    notifyError(error, "打开数据目录失败");
  }
}
