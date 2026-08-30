import {
    ArrowLeft,
    ArrowRight,
    FileArrowUp,
    Trash,
    Waveform,
} from "@phosphor-icons/react";
import { useRef, useState, type ChangeEvent } from "react";

import type {
    CustomWaveformSnapshot,
    WaveformConfig,
} from "../lib/contracts";
import { getErrorMessage } from "../lib/errors";
import { parseWaveformFiles } from "../lib/waveforms";

interface FixedWaveformSourceSettingsProps {
    customWaveforms: CustomWaveformSnapshot[];
    disabled: boolean;
    onDeleteCustomWaveform: (presetId: string) => void;
    onError: (message: string) => void;
    onImportCustomWaveforms: (configs: WaveformConfig[]) => void;
    onReorderCustomWaveforms: (presetIds: string[]) => void;
}

export const FixedWaveformSourceSettings = ({
    customWaveforms,
    disabled,
    onDeleteCustomWaveform,
    onError,
    onImportCustomWaveforms,
    onReorderCustomWaveforms,
}: FixedWaveformSourceSettingsProps) => {
    const fileInputRef = useRef<HTMLInputElement>(null);
    const [pendingDeleteId, setPendingDeleteId] = useState<string | null>(null);

    const handleFiles = async (event: ChangeEvent<HTMLInputElement>) => {
        const files = Array.from(event.target.files ?? []);
        event.target.value = "";
        if (files.length === 0) {
            return;
        }
        try {
            onImportCustomWaveforms(await parseWaveformFiles(files));
        } catch (error) {
            onError(getErrorMessage(error));
        }
    };

    const moveCustomWaveform = (index: number, offset: -1 | 1) => {
        const targetIndex = index + offset;
        if (targetIndex < 0 || targetIndex >= customWaveforms.length) {
            return;
        }
        const ids = customWaveforms.map((waveform) => waveform.id);
        [ids[index], ids[targetIndex]] = [ids[targetIndex], ids[index]];
        onReorderCustomWaveforms(ids);
    };

    return (
        <section
            aria-label="固定波形输入源设置"
            className="fixed-waveform-source-settings"
        >
            <header>
                <div>
                    <strong>自定义波形</strong>
                    <span>固定波形输入源的共享资源；设备通道只负责选择</span>
                </div>
                <input
                    accept=".pulse,.json,.pulses,application/json,text/plain"
                    aria-label="导入自定义波形文件"
                    hidden
                    multiple
                    onChange={(event) => void handleFiles(event)}
                    ref={fileInputRef}
                    type="file"
                />
                <button
                    className="secondary-button custom-waveform-import"
                    disabled={disabled}
                    onClick={() => fileInputRef.current?.click()}
                    type="button"
                >
                    <FileArrowUp aria-hidden="true" size={17} />
                    导入波形
                </button>
            </header>

            {customWaveforms.length === 0 ? (
                <div className="custom-waveform-library-empty">
                    <Waveform aria-hidden="true" size={22} weight="light" />
                    <span>尚未导入波形</span>
                    <small>支持 DG-LAB APP .pulse 与 V3 十六进制帧 JSON</small>
                </div>
            ) : (
                <div className="custom-waveform-library-grid">
                    {customWaveforms.map((waveform, index) => (
                        <article className="custom-waveform-library-card" key={waveform.id}>
                            <div className="custom-waveform-library-icon">
                                <Waveform aria-hidden="true" size={19} />
                            </div>
                            <div className="custom-waveform-library-copy">
                                <strong title={waveform.name}>{waveform.name}</strong>
                                <span>
                                    {waveform.frameCount} 帧 · {(
                                        waveform.durationMs / 1000
                                    ).toFixed(1)} 秒
                                </span>
                            </div>
                            <div className="custom-waveform-library-actions">
                                <button
                                    aria-label={`前移 ${waveform.name}`}
                                    disabled={disabled || index === 0}
                                    onClick={() => moveCustomWaveform(index, -1)}
                                    type="button"
                                >
                                    <ArrowLeft aria-hidden="true" size={15} />
                                </button>
                                <button
                                    aria-label={`后移 ${waveform.name}`}
                                    disabled={
                                        disabled ||
                                        index === customWaveforms.length - 1
                                    }
                                    onClick={() => moveCustomWaveform(index, 1)}
                                    type="button"
                                >
                                    <ArrowRight aria-hidden="true" size={15} />
                                </button>
                                <button
                                    aria-label={
                                        pendingDeleteId === waveform.id
                                            ? `确认删除 ${waveform.name}`
                                            : `删除 ${waveform.name}`
                                    }
                                    className={
                                        pendingDeleteId === waveform.id
                                            ? "confirm-delete"
                                            : ""
                                    }
                                    disabled={disabled}
                                    onClick={() => {
                                        if (pendingDeleteId === waveform.id) {
                                            setPendingDeleteId(null);
                                            onDeleteCustomWaveform(waveform.id);
                                        } else {
                                            setPendingDeleteId(waveform.id);
                                        }
                                    }}
                                    type="button"
                                >
                                    <Trash aria-hidden="true" size={15} />
                                </button>
                            </div>
                        </article>
                    ))}
                </div>
            )}
        </section>
    );
};
