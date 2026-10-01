import { FloppyDisk, Gear } from "@phosphor-icons/react";
import { useEffect, useState } from "react";

import type { BleParameters, DeviceSnapshot } from "../lib/contracts";

interface BleDeviceSettingsProps {
    device: DeviceSnapshot;
    disabled: boolean;
    onSave: (deviceId: string, config: BleParameters) => void;
}

export const BleDeviceSettings = ({
    device,
    disabled,
    onSave,
}: BleDeviceSettingsProps) => {
    const config = device.bleParameters;
    const [draft, setDraft] = useState(config);
    const configKey = JSON.stringify(config);
    useEffect(() => setDraft(config), [configKey]);
    if (!draft || !config) {
        return null;
    }
    const fields: [keyof Omit<BleParameters, "wheelProtectionEnabled">, string, number, number][] = [
        ["maxStrengthA", "A 通道软上限", 0, 200],
        ["maxStrengthB", "B 通道软上限", 0, 200],
        ["frequencyBalanceA", "A 频率平衡", 0, 255],
        ["frequencyBalanceB", "B 频率平衡", 0, 255],
        ["strengthBalanceA", "A 强度平衡", 0, 255],
        ["strengthBalanceB", "B 强度平衡", 0, 255],
        ["wheelProtectionValue", "旋钮保护值", 1, 50],
    ];
    const valid = fields.every(([key, , min, max]) =>
        Number.isInteger(draft[key]) && draft[key] >= min && draft[key] <= max);
    const dirty = JSON.stringify(draft) !== JSON.stringify(config);
    return (
        <details className="ble-device-settings">
            <summary>
                <Gear aria-hidden="true" size={17} />
                蓝牙参数
            </summary>
            <p>
                标准模式 · {device.capabilities.standardMode ? "初始化已确认" : "固件扩展不可用"}。
                基础强度与输出状态不会恢复。
            </p>
            <div className="input-mode-fields">
                {fields.map(([key, label, min, max]) => (
                    <label key={key}>
                        {label}
                        <input
                            aria-label={`${device.name} ${label}`}
                            disabled={disabled || (key === "wheelProtectionValue" && !device.capabilities.wheelProtection)}
                            max={max}
                            min={min}
                            onChange={(event) => setDraft({ ...draft, [key]: event.currentTarget.valueAsNumber })}
                            step={1}
                            type="number"
                            value={Number.isFinite(draft[key]) ? draft[key] : ""}
                        />
                    </label>
                ))}
            </div>
            <label className="input-mode-checkbox">
                <input
                    aria-label={`${device.name} 旋钮保护`}
                    checked={draft.wheelProtectionEnabled}
                    disabled={disabled || !device.capabilities.wheelProtection}
                    onChange={(event) => setDraft({ ...draft, wheelProtectionEnabled: event.currentTarget.checked })}
                    type="checkbox"
                />
                旋钮保护{!device.capabilities.wheelProtection && "（固件不支持）"}
            </label>
            <div className="ble-config-status">
                参数状态：{device.configurationStatus === "sent"
                    ? "已下发（BF 无设备回执）"
                    : device.configurationStatus === "confirmed" ? "设备已确认" : "尚未下发"}
            </div>
            <button
                className="primary-compact-button"
                disabled={disabled || !valid || !dirty || device.initialization !== "ready"}
                onClick={() => onSave(device.controlId, draft)}
                type="button"
            >
                <FloppyDisk aria-hidden="true" size={16} />
                应用并保存参数
            </button>
        </details>
    );
};
