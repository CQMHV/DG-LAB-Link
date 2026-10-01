import { X } from "@phosphor-icons/react";
import { useEffect, useId, useRef, type ReactNode } from "react";

import "./ConnectionManager.css";

export interface DialogProps {
    title: string;
    description?: string;
    onClose: () => void;
    children: ReactNode;
    className?: string;
}

const focusableSelector =
    "button:not([disabled]), a[href], summary, input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex='-1'])";

export const Dialog = ({
    title,
    description,
    onClose,
    children,
    className,
}: DialogProps) => {
    const id = useId();
    const dialogRef = useRef<HTMLElement>(null);
    const closeRef = useRef<HTMLButtonElement>(null);
    const onCloseRef = useRef(onClose);

    useEffect(() => {
        onCloseRef.current = onClose;
    }, [onClose]);

    useEffect(() => {
        const previouslyFocused = document.activeElement instanceof HTMLElement
            ? document.activeElement
            : null;
        closeRef.current?.focus();

        const onKeyDown = (event: KeyboardEvent) => {
            if (event.key === "Escape") {
                event.preventDefault();
                onCloseRef.current();
                return;
            }
            if (event.key !== "Tab") {
                return;
            }

            const focusable = [
                ...Array.from(dialogRef.current?.querySelectorAll<HTMLElement>(focusableSelector) ?? []),
                ...Array.from(document.querySelectorAll<HTMLButtonElement>("button[data-emergency-stop]:not([disabled])")),
            ].filter((element, index, elements) =>
                element.tabIndex >= 0 &&
                element.getClientRects().length > 0 &&
                elements.indexOf(element) === index);

            event.preventDefault();
            if (focusable.length === 0) {
                dialogRef.current?.focus();
                return;
            }

            const current = focusable.findIndex((element) => element === document.activeElement);
            const next = current < 0
                ? (event.shiftKey ? focusable.length - 1 : 0)
                : (current + (event.shiftKey ? -1 : 1) + focusable.length) % focusable.length;
            focusable[next].focus();
        };

        window.addEventListener("keydown", onKeyDown);
        return () => {
            window.removeEventListener("keydown", onKeyDown);
            if (previouslyFocused?.isConnected) {
                previouslyFocused.focus();
            }
        };
    }, []);

    return (
        <div
            className="app-dialog-backdrop"
            onMouseDown={(event) => {
                if (event.target === event.currentTarget) {
                    onClose();
                }
            }}
        >
            <section
                aria-describedby={description ? `${id}-description` : undefined}
                aria-labelledby={`${id}-title`}
                aria-modal="true"
                className={`app-dialog${className ? ` ${className}` : ""}`}
                ref={dialogRef}
                role="dialog"
                tabIndex={-1}
            >
                <header className="app-dialog-heading">
                    <div>
                        <h2 id={`${id}-title`}>{title}</h2>
                        {description && <p id={`${id}-description`}>{description}</p>}
                    </div>
                    <button
                        aria-label={`关闭${title}窗口`}
                        className="icon-button app-dialog-close"
                        onClick={onClose}
                        ref={closeRef}
                        type="button"
                    >
                        <X aria-hidden="true" size={20} />
                    </button>
                </header>
                <div className="app-dialog-body">{children}</div>
            </section>
        </div>
    );
};
