interface SerializedCommandError {
    code?: unknown;
    message?: unknown;
}

const isSerializedCommandError = (
    error: unknown,
): error is SerializedCommandError =>
    typeof error === "object" && error !== null;

export const getErrorMessage = (
    error: unknown,
    fallback = "操作失败，请稍后重试",
): string => {
    if (error instanceof Error) {
        return error.message;
    }
    if (typeof error === "string") {
        return error;
    }
    if (isSerializedCommandError(error)) {
        const message =
            typeof error.message === "string" ? error.message.trim() : "";
        const code = typeof error.code === "string" ? error.code.trim() : "";

        if (message && code) {
            return `${message}（${code}）`;
        }
        if (message) {
            return message;
        }
        if (code) {
            return `${fallback}（${code}）`;
        }
    }
    return fallback;
};
