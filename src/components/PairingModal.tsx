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
    controllerId: string | null;
    pairingUrl: string;
    onClose: () => void;
}

export const PairingModal = ({
    controllerId,
    pairingUrl,
    onClose,
}: PairingModalProps) => {
    const modalRef = useRef<HTMLElement>(null);
    const closeButtonRef = useRef<HTMLButtonElement>(null);
    const [qrCode, setQrCode] = useState<string | null>(null);
    const [error, setError] = useState<string | null>(null);
    const [copied, setCopied] = useState(false);

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

    const copyPairingUrl = async () => {
        try {
            await navigator.clipboard.writeText(pairingUrl);
            setCopied(true);
            window.setTimeout(() => setCopied(false), 1600);
        } catch {
            setError("无法访问剪贴板，请手动复制配对链接");
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
                        <span className="eyebrow">DG-LAB SOCKET V4</span>
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

                <p className="modal-description">
                    使用 DG-LAB APP 扫描二维码，中枢会自动接收 APP 与设备状态。
                </p>

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

                {controllerId && (
                    <div className="pairing-id">
                        <span>控制端 ID</span>
                        <code>{controllerId}</code>
                    </div>
                )}

                <div className="pairing-link-row">
                    <LinkSimple aria-hidden="true" size={18} />
                    <span title={pairingUrl}>{pairingUrl}</span>
                    <button
                        aria-label="复制配对链接"
                        className="copy-button"
                        onClick={() => void copyPairingUrl()}
                        type="button"
                    >
                        {copied ? (
                            <Check aria-hidden="true" size={18} />
                        ) : (
                            <Copy aria-hidden="true" size={18} />
                        )}
                    </button>
                </div>

                {error && <p className="inline-error">{error}</p>}
            </section>
        </div>
    );
};
