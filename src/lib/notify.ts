import { toast } from "vue-sonner";

export function toErrorMessage(error: unknown, fallback = "操作失败"): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return fallback;
}

export function notifyError(error: unknown, fallback?: string): void {
  toast.error(toErrorMessage(error, fallback));
}

export function notifySuccess(message: string, description?: string): void {
  toast.success(message, description ? { description } : undefined);
}

export function notifyInfo(message: string, description?: string): void {
  toast.info(message, description ? { description } : undefined);
}

export async function attempt<T>(
  action: () => Promise<T>,
  options: { success?: string; error?: string } = {},
): Promise<T | undefined> {
  try {
    const result = await action();
    if (options.success) notifySuccess(options.success);
    return result;
  } catch (error) {
    notifyError(error, options.error);
    return undefined;
  }
}
