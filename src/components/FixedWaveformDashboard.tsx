import { Check, Waveform } from "@phosphor-icons/react";

import type {
    CustomWaveformSnapshot,
    HubChannel,
    WaveformConfig,
} from "../lib/contracts";
import {
    findOfficialWaveform,
    OFFICIAL_WAVEFORMS,
} from "../lib/waveforms";

interface FixedWaveformChannelDashboardProps {
    channel: HubChannel;
    customWaveforms: CustomWaveformSnapshot[];
    disabled: boolean;
    selectedId: string | null;
    selectedName: string | null;
    onSelectCustomWaveform: (presetId: string) => void;
    onSetFixedWaveform: (config: WaveformConfig) => void;
}

export const FixedWaveformChannelDashboard = ({
    channel,
    customWaveforms,
    disabled,
    selectedId,
    selectedName,
    onSelectCustomWaveform,
    onSetFixedWaveform,
}: FixedWaveformChannelDashboardProps) => {
    const channelName = channel.toUpperCase();
    const selectOfficialWaveform = (presetId: string) => {
        const waveform = findOfficialWaveform(presetId);
        onSetFixedWaveform({
            presetId: waveform.presetId,
            presetName: waveform.presetName,
            frames: [...waveform.frames],
        });
    };

    return (
        <section
            aria-label={`${channelName} 通道固定波形仪表盘`}
            className="waveform-palette"
        >
            <header className="waveform-palette-header">
                <div className="waveform-palette-title">
                    <Waveform aria-hidden="true" size={18} weight="light" />
                    <strong>固定波形</strong>
                </div>
                <span className="waveform-palette-current">
                    {selectedName ?? "无波形"}
                </span>
            </header>

            <div className="waveform-palette-group">
                <div className="waveform-palette-group-title">
                    <span>内置波形</span>
                    <small>{OFFICIAL_WAVEFORMS.length}</small>
                </div>
                <div
                    aria-label={`${channelName} 通道内置波形`}
                    className="waveform-choice-grid"
                    role="group"
                >
                    {OFFICIAL_WAVEFORMS.map((waveform) => {
                        const selected = selectedId === waveform.presetId;
                        return (
                            <button
                                aria-label={`${channelName} 通道选择 ${waveform.presetName}`}
                                aria-pressed={selected}
                                className="waveform-choice"
                                data-selected={selected}
                                disabled={disabled}
                                key={waveform.presetId}
                                onClick={() =>
                                    selectOfficialWaveform(waveform.presetId)
                                }
                                type="button"
                            >
                                <span className="waveform-choice-glyph">
                                    <Waveform aria-hidden="true" size={23} weight="duotone" />
                                </span>
                                <span
                                    className="waveform-choice-label"
                                    title={waveform.presetName}
                                >
                                    {waveform.presetName}
                                </span>
                                {selected && (
                                    <span className="waveform-choice-check">
                                        <Check
                                            aria-hidden="true"
                                            size={11}
                                            weight="bold"
                                        />
                                    </span>
                                )}
                            </button>
                        );
                    })}
                </div>
            </div>

            <div className="waveform-palette-group">
                <div className="waveform-palette-group-title">
                    <span>自定义波形</span>
                    <small>{customWaveforms.length}</small>
                </div>
                {customWaveforms.length > 0 ? (
                    <div
                        aria-label={`${channelName} 通道自定义波形`}
                        className="waveform-choice-grid"
                        role="group"
                    >
                        {customWaveforms.map((waveform) => {
                            const selected = selectedId === waveform.id;
                            return (
                                <button
                                    aria-label={`${channelName} 通道选择 ${waveform.name}`}
                                    aria-pressed={selected}
                                    className="waveform-choice"
                                    data-selected={selected}
                                    disabled={disabled}
                                    key={waveform.id}
                                    onClick={() =>
                                        onSelectCustomWaveform(waveform.id)
                                    }
                                    type="button"
                                >
                                    <span className="waveform-choice-glyph custom">
                                        <Waveform aria-hidden="true" size={23} weight="duotone" />
                                    </span>
                                    <span
                                        className="waveform-choice-label"
                                        title={waveform.name}
                                    >
                                        {waveform.name}
                                    </span>
                                    {selected && (
                                        <span className="waveform-choice-check">
                                            <Check
                                                aria-hidden="true"
                                                size={11}
                                                weight="bold"
                                            />
                                        </span>
                                    )}
                                </button>
                            );
                        })}
                    </div>
                ) : (
                    <div className="waveform-palette-empty">
                        尚未导入自定义波形
                    </div>
                )}
            </div>
        </section>
    );
};
