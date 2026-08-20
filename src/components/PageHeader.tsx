import type { ReactNode } from "react";

interface PageHeaderProps {
    eyebrow: string;
    title: string;
    description: string;
    actions?: ReactNode;
}

export const PageHeader = ({
    eyebrow,
    title,
    description,
    actions,
}: PageHeaderProps) => (
    <header className="page-header">
        <div>
            <span className="eyebrow">{eyebrow}</span>
            <h1>{title}</h1>
            <p>{description}</p>
        </div>
        {actions && <div className="page-header-actions">{actions}</div>}
    </header>
);
