import {
    CheckCircle,
    Info,
    Warning,
    XCircle,
} from "@phosphor-icons/react";
import { useState } from "react";

import { PageHeader } from "../components/PageHeader";
import type { HubSnapshot, LogLevel } from "../lib/contracts";

type LogFilter = "all" | LogLevel;

interface LogsPageProps {
    snapshot: HubSnapshot;
}

const logIcon = {
    info: Info,
    warning: Warning,
    error: XCircle,
};

export const LogsPage = ({ snapshot }: LogsPageProps) => {
    const [filter, setFilter] = useState<LogFilter>("all");
    const visibleLogs = snapshot.logs.filter(
        (log) => filter === "all" || log.level === filter,
    );

    return (
        <div className="standard-page">
            <PageHeader
                description="连接、输入源、输出和安全操作都会记录在本机，不包含连续原始波形数据。"
                eyebrow="RUNTIME LOG"
                title="运行记录"
            />

            <div className="log-toolbar" aria-label="日志筛选">
                {(
                    [
                        ["all", "全部"],
                        ["info", "信息"],
                        ["warning", "警告"],
                        ["error", "错误"],
                    ] as const
                ).map(([value, label]) => (
                    <button
                        aria-pressed={filter === value}
                        className={filter === value ? "filter-active" : ""}
                        key={value}
                        onClick={() => setFilter(value)}
                        type="button"
                    >
                        {label}
                    </button>
                ))}
                <span>{visibleLogs.length} 条记录</span>
            </div>

            <section className="log-list">
                {visibleLogs.length > 0 ? (
                    visibleLogs.map((log) => {
                        const LogIcon = logIcon[log.level];
                        return (
                            <article className={`log-row log-${log.level}`} key={log.id}>
                                <LogIcon aria-hidden="true" size={19} />
                                <time dateTime={log.timestamp}>
                                    {new Intl.DateTimeFormat("zh-CN", {
                                        hour: "2-digit",
                                        minute: "2-digit",
                                        second: "2-digit",
                                    }).format(new Date(log.timestamp))}
                                </time>
                                <span>{log.message}</span>
                            </article>
                        );
                    })
                ) : (
                    <div className="empty-log">
                        <CheckCircle aria-hidden="true" size={28} />
                        当前筛选下没有记录
                    </div>
                )}
            </section>
        </div>
    );
};
