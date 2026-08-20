import { Minus, Square, X } from "@phosphor-icons/react";

import { performWindowAction } from "../lib/bridge";

interface WindowChromeProps {
    title?: string;
}

export const WindowChrome = ({ title = "DG-LAB Link" }: WindowChromeProps) => (
    <header className="window-chrome" data-tauri-drag-region>
        <div className="window-brand" data-tauri-drag-region>
            {title}
        </div>
        <div className="window-drag-region" data-tauri-drag-region />
        <div className="window-actions">
            <button
                aria-label="最小化窗口"
                className="window-action"
                onClick={() => void performWindowAction("minimize")}
                type="button"
            >
                <Minus aria-hidden="true" size={18} weight="light" />
            </button>
            <button
                aria-label="最大化或还原窗口"
                className="window-action"
                onClick={() => void performWindowAction("toggleMaximize")}
                type="button"
            >
                <Square aria-hidden="true" size={16} weight="light" />
            </button>
            <button
                aria-label="关闭窗口"
                className="window-action window-action-close"
                onClick={() => void performWindowAction("close")}
                type="button"
            >
                <X aria-hidden="true" size={18} weight="light" />
            </button>
        </div>
    </header>
);
