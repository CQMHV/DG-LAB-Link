import { Plus, Trash } from "@phosphor-icons/react";
import type { MappingPoint } from "../lib/contracts";

interface MappingCurveEditorProps {
    label: string;
    points: MappingPoint[];
    min: number;
    max: number;
    unit: string;
    disabled?: boolean;
    onChange: (points: MappingPoint[]) => void;
}

export const MappingCurveEditor = ({ label, points, min, max, unit, disabled = false, onChange }: MappingCurveEditorProps) => {
    const change = (index: number, field: "x" | "y", value: number) => {
        onChange(points.map((point, item) => item === index ? { ...point, [field]: value } : point));
    };
    const addPoint = () => {
        let widest = 0;
        for (let index = 1; index < points.length - 1; index += 1) {
            if (points[index + 1].x - points[index].x > points[widest + 1].x - points[widest].x) widest = index;
        }
        const left = points[widest];
        const right = points[widest + 1];
        const next = [...points];
        next.splice(widest + 1, 0, { x: Number(((left.x + right.x) / 2).toFixed(3)), y: Math.round((left.y + right.y) / 2) });
        onChange(next);
    };
    return (
        <fieldset className="mapping-editor" disabled={disabled}>
            <legend>{label}</legend>
            <p className="input-mode-note">位置 0–1，节点间线性变化；最多六个节点。</p>
            <div className="mapping-node-list">
                {points.map((point, index) => (
                    <div className="mapping-node" key={index}>
                        <label>位置<input aria-label={`${label} 节点 ${index + 1} 位置`} disabled={index === 0 || index === points.length - 1} min={index > 0 ? points[index - 1].x + 0.001 : 0} max={index < points.length - 1 ? points[index + 1].x - 0.001 : 1} onChange={(event) => change(index, "x", event.currentTarget.valueAsNumber)} step="0.01" type="number" value={Number.isFinite(point.x) ? point.x : ""} /></label>
                        <label>{unit}<input aria-label={`${label} 节点 ${index + 1} 数值`} min={min} max={max} onChange={(event) => change(index, "y", event.currentTarget.valueAsNumber)} type="number" value={Number.isFinite(point.y) ? point.y : ""} /></label>
                        <button aria-label={`删除 ${label} 节点 ${index + 1}`} className="icon-button" disabled={index === 0 || index === points.length - 1 || points.length <= 2} onClick={() => onChange(points.filter((_, item) => item !== index))} type="button"><Trash aria-hidden="true" size={15} /></button>
                    </div>
                ))}
            </div>
            <button className="secondary-button" disabled={points.length >= 6} onClick={addPoint} type="button"><Plus aria-hidden="true" size={15} />添加节点</button>
        </fieldset>
    );
};
