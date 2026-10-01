import {
    Check,
    Copy,
    LinkSimple,
    SpinnerGap,
    X,
} from "@phosphor-icons/react";
import QRCode from "qrcode";
import { useEffect, useRef, useState } from "react";

interface PairingModalProps {
    protocol?: "v4" | "v3";
    controllerId: string | null;
    pairingUrl: string;
    onClose: () => void;
}

export const PairingModal = ({
    protocol = "v4",
    controllerId,
    pairingUrl,
    onClose,
}: PairingModalProps) => {
    const modalRef = useRef<HTMLElement>(null);
    const closeButtonRef = useRef<HTMLButtonElement>(null);
    const [qrCode, setQrCode] = useState<string | null>(null);
    const [error, setError] = useState<string | null>(null);
    const [copiedTarget, setCopiedTarget] = useState<
        "controllerId" | "pairingUrl" | null
    >(null);

    useEffect(() => {
        let active = true;

        const generateQrCode = async () => {
            try {
                const dataUrl = await QRCode.toDataURL(pairingUrl, {
                    width: 280,
                    margin: 2,
                    errorCorrectionLevel: "M",
                    color: {
                        dark: "#171713",
                        light: "#f4d67b",
                    },
                });
                if (active) {
                    setQrCode(dataUrl);
                    setError(null);
                }
            } catch {
                if (active) {
                    setError("二维码生成失败，请复制链接后在 APP 中打开");
                }
            }
        };

        void generateQrCode();

        return () => {
            active = false;
        };
    }, [pairingUrl]);

    useEffect(() => {
        const previouslyFocused =
            document.activeElement instanceof HTMLElement
                ? document.activeElement
                : null;

        closeButtonRef.current?.focus();

        const onKeyDown = (event: KeyboardEvent) => {
            if (event.key === "Escape") {
                event.preventDefault();
                onClose();
                return;
            }
            if (event.key !== "Tab") {
                return;
            }

            const modal = modalRef.current;
            const focusable = Array.from(
                modal?.querySelectorAll<HTMLElement>(
                    "button:not([disabled]), a[href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex='-1'])",
                ) ?? [],
            );
            if (focusable.length === 0) {
                event.preventDefault();
                return;
            }

            const first = focusable[0];
            const last = focusable[focusable.length - 1];
            if (
                event.shiftKey &&
                (document.activeElement === first || !modal?.contains(document.activeElement))
            ) {
                event.preventDefault();
                last.focus();
            } else if (
                !event.shiftKey &&
                (document.activeElement === last || !modal?.contains(document.activeElement))
            ) {
                event.preventDefault();
                first.focus();
            }
        };

        window.addEventListener("keydown", onKeyDown);
        return () => {
            window.removeEventListener("keydown", onKeyDown);
            if (previouslyFocused?.isConnected) {
                previouslyFocused.focus();
            }
        };
    }, [onClose]);

    const copyValue = async (
        value: string,
        target: "controllerId" | "pairingUrl",
    ) => {
        try {
            await navigator.clipboard.writeText(value);
            setCopiedTarget(target);
            window.setTimeout(() => {
                setCopiedTarget((current) =>
                    current === target ? null : current,
                );
            }, 1600);
        } catch {
            setError(
                target === "controllerId"
                    ? "无法访问剪贴板，请手动记录控制端 ID"
                    : "无法访问剪贴板，请手动复制配对链接",
            );
        }
    };

    return (
        <div
            aria-label="配对 APP"
            aria-modal="true"
            className="modal-backdrop"
            onMouseDown={(event) => {
                if (event.target === event.currentTarget) {
                    onClose();
                }
            }}
            role="dialog"
        >
            <section className="pairing-modal" ref={modalRef}>
                <div className="modal-heading">
                    <div>
                        <span className="eyebrow">DG-LAB SOCKET {protocol.toUpperCase()}</span>
                        <h2>配对 DG-LAB APP</h2>
                    </div>
                    <button
                        aria-label="关闭配对窗口"
                        className="icon-button"
                        onClick={onClose}
                        ref={closeButtonRef}
                        type="button"
                    >
                        <X aria-hidden="true" size={20} />
                    </button>
                </div>

                <div className="qr-frame">
                    {qrCode ? (
                        <img alt="DG-LAB APP 配对二维码" src={qrCode} />
                    ) : (
                        <div className="qr-loading">
                            <SpinnerGap
                                aria-hidden="true"
                                className="spin"
                                size={28}
                            />
                            正在生成二维码
                        </div>
                    )}
                </div>

                <div className="pairing-meta-row">
                    <span>控制端 ID</span>
                    <code>{controllerId ?? "等待分配"}</code>
                    <div className="pairing-copy-actions">
                        <button
                            aria-label="复制控制端 ID"
                            className="copy-button"
                            disabled={!controllerId}
                            onClick={() =>
                                controllerId &&
                                void copyValue(controllerId, "controllerId")
                            }
                            title="复制控制端 ID"
                            type="button"
                        >
                            {copiedTarget === "controllerId" ? (
                                <Check aria-hidden="true" size={18} />
                            ) : (
                                <Copy aria-hidden="true" size={18} />
                            )}
                        </button>
                        <button
                            aria-label="复制配对链接"
                            className="copy-button"
                            onClick={() =>
                                void copyValue(pairingUrl, "pairingUrl")
                            }
                            title="复制配对链接"
                            type="button"
                        >
                            {copiedTarget === "pairingUrl" ? (
                                <Check aria-hidden="true" size={18} />
                            ) : (
                                <LinkSimple aria-hidden="true" size={18} />
                            )}
                        </button>
                    </div>
                </div>

                {error && <p className="inline-error">{error}</p>}
            </section>
        </div>
    );
};
