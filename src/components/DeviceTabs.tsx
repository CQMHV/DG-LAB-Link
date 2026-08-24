import {
    CaretDown,
    Check,
    MagnifyingGlass,
    Plus,
    X,
} from "@phosphor-icons/react";
import {
    useEffect,
    useRef,
    useState,
    type PointerEvent as ReactPointerEvent,
} from "react";
import { createPortal } from "react-dom";

import type { DeviceSnapshot } from "../lib/contracts";

export interface DeviceViewTab {
    id: string;
    deviceId: string | null;
}

interface DeviceTabsProps {
    activeTabId: string;
    detachedTabs: DeviceViewTab[];
    devices: DeviceSnapshot[];
    onClose: (tabId: string) => void;
    onDetach: (tab: DeviceViewTab) => void;
    onMove: (tabId: string, targetTabId: string) => void;
    onNewTab: () => void;
    onFocusDetached: (tabId: string) => void;
    onSelect: (tabId: string) => void;
    pendingAction: string | null;
    tabs: DeviceViewTab[];
}

interface DragPreview {
    height: number;
    tabId: string;
    width: number;
    x: number;
    y: number;
}

interface DeviceTabLabelProps {
    device?: DeviceSnapshot;
}

const DETACH_DISTANCE = 72;
const DRAG_START_DISTANCE = 6;
const TAB_MOVE_DURATION = 150;
const TAB_MOVE_EASING = "cubic-bezier(0.23, 1, 0.32, 1)";

const shouldDetachFromTrack = (
    clientX: number,
    clientY: number,
    origin: { x: number; y: number },
    trackBounds: DOMRect | null,
) =>
    Boolean(
        trackBounds &&
            (clientX < trackBounds.left ||
                clientX > trackBounds.right ||
                clientY < trackBounds.top ||
                clientY > trackBounds.bottom) &&
            Math.hypot(clientX - origin.x, clientY - origin.y) >=
                DETACH_DISTANCE,
    );

const DeviceTabLabel = ({ device }: DeviceTabLabelProps) => (
    <span className="device-tab-name">{device?.name ?? "新标签页"}</span>
);

export const DeviceTabs = ({
    activeTabId,
    detachedTabs,
    devices,
    onClose,
    onDetach,
    onMove,
    onNewTab,
    onFocusDetached,
    onSelect,
    pendingAction,
    tabs,
}: DeviceTabsProps) => {
    const [dragPreview, setDragPreview] = useState<DragPreview | null>(null);
    const [tabListOpen, setTabListOpen] = useState(false);
    const [tabQuery, setTabQuery] = useState("");
    const draggingTabIdRef = useRef<string | null>(null);
    const activePointerId = useRef<number | null>(null);
    const pointerOrigin = useRef({ x: 0, y: 0 });
    const dragRect = useRef<DOMRect | null>(null);
    const dragTrackRect = useRef<DOMRect | null>(null);
    const dragTab = useRef<DeviceViewTab | null>(null);
    const dragElement = useRef<HTMLElement | null>(null);
    const grabOffset = useRef({ x: 0, y: 0 });
    const suppressClickTabId = useRef<string | null>(null);
    const tabListButtonRef = useRef<HTMLButtonElement | null>(null);
    const tabListRef = useRef<HTMLDivElement | null>(null);
    const tabSearchRef = useRef<HTMLInputElement | null>(null);
    const tabsRef = useRef(tabs);
    const pointerMoveHandler = useRef<(event: PointerEvent) => void>(() => {});
    const pointerUpHandler = useRef<(event: PointerEvent) => void>(() => {});
    const pointerCancelHandler = useRef<(event: PointerEvent) => void>(
        () => {},
    );
    tabsRef.current = tabs;

    const resetDrag = () => {
        activePointerId.current = null;
        draggingTabIdRef.current = null;
        dragRect.current = null;
        dragTrackRect.current = null;
        dragTab.current = null;
        dragElement.current = null;
        setDragPreview(null);
    };

    const moveTab = (tabId: string, targetTabId: string) => {
        const previousPositions = new Map(
            Array.from(
                document.querySelectorAll<HTMLElement>(
                    ".device-tabs [data-device-tab-id]",
                ),
            ).map((element) => [
                element.dataset.deviceTabId,
                element.getBoundingClientRect().left,
            ]),
        );
        onMove(tabId, targetTabId);
        requestAnimationFrame(() => {
            document
                .querySelectorAll<HTMLElement>(
                    ".device-tabs [data-device-tab-id]",
                )
                .forEach((element) => {
                    if (element.dataset.deviceTabId === tabId) {
                        return;
                    }
                    const previousLeft = previousPositions.get(
                        element.dataset.deviceTabId,
                    );
                    if (previousLeft === undefined) {
                        return;
                    }
                    const delta =
                        previousLeft - element.getBoundingClientRect().left;
                    if (Math.abs(delta) < 1) {
                        return;
                    }
                    element.animate?.(
                        [
                            { transform: `translateX(${delta}px)` },
                            { transform: "translateX(0)" },
                        ],
                        {
                            duration: TAB_MOVE_DURATION,
                            easing: TAB_MOVE_EASING,
                        },
                    );
                });
        });
    };

    pointerMoveHandler.current = (event) => {
        if (activePointerId.current !== event.pointerId) {
            return;
        }
        const tab = dragTab.current;
        const bounds = dragRect.current;
        if (!tab || !bounds) {
            return;
        }
        const distance = Math.hypot(
            event.clientX - pointerOrigin.current.x,
            event.clientY - pointerOrigin.current.y,
        );
        if (!draggingTabIdRef.current && distance < DRAG_START_DISTANCE) {
            return;
        }
        event.preventDefault();
        const rawPreviewX = event.clientX - grabOffset.current.x;
        const trackBounds = dragTrackRect.current;
        const outsideTabTrack = shouldDetachFromTrack(
            event.clientX,
            event.clientY,
            pointerOrigin.current,
            trackBounds,
        );
        const previewX =
            outsideTabTrack || !trackBounds
                ? rawPreviewX
                : Math.min(
                      Math.max(rawPreviewX, trackBounds.left),
                      Math.max(
                          trackBounds.left,
                          trackBounds.right - bounds.width,
                      ),
                  );
        const previewY = outsideTabTrack
            ? event.clientY - grabOffset.current.y
            : bounds.top;
        if (!draggingTabIdRef.current) {
            draggingTabIdRef.current = tab.id;
            dragElement.current?.setPointerCapture?.(event.pointerId);
            setDragPreview({
                height: bounds.height,
                tabId: tab.id,
                width: bounds.width,
                x: previewX,
                y: previewY,
            });
        } else {
            setDragPreview((current) =>
                current
                    ? {
                          ...current,
                          x: previewX,
                          y: previewY,
                      }
                    : current,
            );
        }

        if (outsideTabTrack) {
            return;
        }
        const previewCenterX = previewX + bounds.width / 2;
        const previewCenterY = bounds.top + bounds.height / 2;
        const targetTab = document
            .elementFromPoint?.(previewCenterX, previewCenterY)
            ?.closest<HTMLElement>("[data-device-tab-id]");
        const targetTabId = targetTab?.dataset.deviceTabId;
        if (!targetTabId || targetTabId === tab.id) {
            return;
        }
        const currentTabs = tabsRef.current;
        const tabIndex = currentTabs.findIndex(
            (candidate) => candidate.id === tab.id,
        );
        const targetIndex = currentTabs.findIndex(
            (candidate) => candidate.id === targetTabId,
        );
        const targetBounds = targetTab.getBoundingClientRect();
        const crossedTargetMiddle =
            tabIndex < targetIndex
                ? previewCenterX >= targetBounds.left + targetBounds.width / 2
                : previewCenterX <= targetBounds.left + targetBounds.width / 2;
        if (crossedTargetMiddle) {
            moveTab(tab.id, targetTabId);
        }
    };

    pointerUpHandler.current = (event) => {
        if (activePointerId.current !== event.pointerId) {
            return;
        }
        const tab = dragTab.current;
        const wasDragging = draggingTabIdRef.current === tab?.id;
        const shouldDetach =
            wasDragging &&
            shouldDetachFromTrack(
                event.clientX,
                event.clientY,
                pointerOrigin.current,
                dragTrackRect.current,
            );
        if (tab && wasDragging) {
            suppressClickTabId.current = tab.id;
            requestAnimationFrame(() => {
                if (suppressClickTabId.current === tab.id) {
                    suppressClickTabId.current = null;
                }
            });
        }
        const pointerId = activePointerId.current;
        if (pointerId !== null && dragElement.current?.hasPointerCapture?.(pointerId)) {
            dragElement.current.releasePointerCapture(pointerId);
        }
        resetDrag();
        if (tab && shouldDetach) {
            onDetach(tab);
        }
    };

    pointerCancelHandler.current = (event) => {
        if (activePointerId.current === event.pointerId) {
            resetDrag();
        }
    };

    useEffect(() => {
        const handlePointerMove = (event: PointerEvent) =>
            pointerMoveHandler.current(event);
        const handlePointerUp = (event: PointerEvent) =>
            pointerUpHandler.current(event);
        const handlePointerCancel = (event: PointerEvent) =>
            pointerCancelHandler.current(event);
        const handleWindowBlur = () => resetDrag();
        window.addEventListener("pointermove", handlePointerMove, {
            passive: false,
        });
        window.addEventListener("pointerup", handlePointerUp);
        window.addEventListener("pointercancel", handlePointerCancel);
        window.addEventListener("blur", handleWindowBlur);
        return () => {
            window.removeEventListener("pointermove", handlePointerMove);
            window.removeEventListener("pointerup", handlePointerUp);
            window.removeEventListener("pointercancel", handlePointerCancel);
            window.removeEventListener("blur", handleWindowBlur);
        };
    }, []);

    useEffect(() => {
        const handleShortcut = (event: KeyboardEvent) => {
            if (
                (event.ctrlKey || event.metaKey) &&
                event.shiftKey &&
                event.key.toLowerCase() === "a"
            ) {
                event.preventDefault();
                setTabListOpen(true);
            }
        };
        window.addEventListener("keydown", handleShortcut);
        return () => window.removeEventListener("keydown", handleShortcut);
    }, []);

    useEffect(() => {
        if (!tabListOpen) {
            return;
        }
        tabSearchRef.current?.focus();
        const handlePointerDown = (event: PointerEvent) => {
            if (!tabListRef.current?.contains(event.target as Node)) {
                setTabListOpen(false);
                setTabQuery("");
            }
        };
        const handleKeyDown = (event: KeyboardEvent) => {
            if (event.key === "Escape") {
                event.preventDefault();
                setTabListOpen(false);
                setTabQuery("");
                tabListButtonRef.current?.focus();
            }
        };
        document.addEventListener("pointerdown", handlePointerDown);
        document.addEventListener("keydown", handleKeyDown);
        return () => {
            document.removeEventListener("pointerdown", handlePointerDown);
            document.removeEventListener("keydown", handleKeyDown);
        };
    }, [tabListOpen]);

    const beginDrag = (
        tab: DeviceViewTab,
        pending: boolean,
        event: ReactPointerEvent<HTMLDivElement>,
    ) => {
        if (
            pending ||
            event.button !== 0 ||
            activePointerId.current !== null
        ) {
            return;
        }
        const element = event.currentTarget;
        const bounds = element.getBoundingClientRect();
        activePointerId.current = event.pointerId;
        pointerOrigin.current = {
            x: event.clientX,
            y: event.clientY,
        };
        dragRect.current = bounds;
        dragTrackRect.current =
            element.closest(".device-tabs")?.getBoundingClientRect() ?? bounds;
        dragTab.current = tab;
        dragElement.current = element;
        grabOffset.current = {
            x: event.clientX - bounds.left,
            y: event.clientY - bounds.top,
        };
        onSelect(tab.id);
    };

    const previewTab = dragPreview
        ? tabs.find((tab) => tab.id === dragPreview.tabId)
        : undefined;
    const previewDevice = devices.find(
        (device) => device.controlId === previewTab?.deviceId,
    );
    const normalizedQuery = tabQuery.trim().toLocaleLowerCase();
    const tabListItems = [
        ...tabs.map((tab) => ({ detached: false, tab })),
        ...detachedTabs.map((tab) => ({ detached: true, tab })),
    ]
        .map(({ detached, tab }) => {
            const device = devices.find(
                (candidate) => candidate.controlId === tab.deviceId,
            );
            return {
                detached,
                name: device?.name ?? "新标签页",
                tab,
            };
        })
        .filter(({ name }) =>
            name.toLocaleLowerCase().includes(normalizedQuery),
        );

    return (
        <div className="device-tabs-shell">
            <div className="device-tab-list" ref={tabListRef}>
                <button
                    aria-controls="device-tab-list-dialog"
                    aria-expanded={tabListOpen}
                    aria-haspopup="dialog"
                    aria-label="搜索标签页"
                    className="device-tab-list-button"
                    onClick={() => {
                        setTabListOpen((open) => !open);
                        if (tabListOpen) {
                            setTabQuery("");
                        }
                    }}
                    ref={tabListButtonRef}
                    title="搜索标签页 (Ctrl+Shift+A)"
                    type="button"
                >
                    <CaretDown aria-hidden="true" size={17} weight="bold" />
                </button>
                {tabListOpen && (
                    <div
                        aria-label="标签页列表"
                        className="device-tab-list-dialog"
                        id="device-tab-list-dialog"
                        role="dialog"
                    >
                        <label className="device-tab-search">
                            <MagnifyingGlass
                                aria-hidden="true"
                                size={17}
                                weight="light"
                            />
                            <span className="visually-hidden">搜索标签页</span>
                            <input
                                aria-label="搜索标签页"
                                onChange={(event) =>
                                    setTabQuery(event.currentTarget.value)
                                }
                                placeholder="搜索标签页"
                                ref={tabSearchRef}
                                type="search"
                                value={tabQuery}
                            />
                            <kbd>Ctrl+Shift+A</kbd>
                        </label>
                        <div className="device-tab-list-heading">打开的标签页</div>
                        <div className="device-tab-list-results">
                            {tabListItems.length > 0 ? (
                                tabListItems.map(({ detached, name, tab }) => (
                                    <button
                                        aria-label={`${detached ? "聚焦窗口标签页" : "切换到标签页"}：${name}`}
                                        className={`device-tab-list-option ${!detached && tab.id === activeTabId ? "device-tab-list-option-active" : ""}`}
                                        key={tab.id}
                                        onClick={() => {
                                            if (detached) {
                                                onFocusDetached(tab.id);
                                            } else {
                                                onSelect(tab.id);
                                            }
                                            setTabListOpen(false);
                                            setTabQuery("");
                                            tabListButtonRef.current?.focus();
                                        }}
                                        type="button"
                                    >
                                        <span>{name}</span>
                                        {!detached && tab.id === activeTabId && (
                                            <Check
                                                aria-hidden="true"
                                                size={16}
                                                weight="bold"
                                            />
                                        )}
                                    </button>
                                ))
                            ) : (
                                <p className="device-tab-list-empty">
                                    {tabs.length + detachedTabs.length > 0
                                        ? "没有匹配的标签页"
                                        : "没有打开的标签页"}
                                </p>
                            )}
                        </div>
                    </div>
                )}
            </div>
            <div aria-label="设备视图" className="device-tabs" role="tablist">
                {tabs.map((tab) => {
                    const device = devices.find(
                        (candidate) => candidate.controlId === tab.deviceId,
                    );
                    const active = tab.id === activeTabId;
                    const pending =
                        pendingAction === `window-${tab.id}` ||
                        (device
                            ? pendingAction === `device-${device.controlId}`
                            : false);
                    const dragging = dragPreview?.tabId === tab.id;

                    return (
                        <div
                            className={`device-tab-item ${active ? "device-tab-active" : ""} ${dragging ? "device-tab-placeholder" : ""}`}
                            data-device-tab-id={tab.id}
                            key={tab.id}
                            onPointerDown={(event) =>
                                beginDrag(tab, pending, event)
                            }
                            title="拖动调整顺序；拖出标签栏以打开独立窗口"
                        >
                            <button
                                aria-controls="active-device-workspace"
                                aria-selected={active}
                                className="device-tab-select"
                                disabled={pending}
                                onClick={(event) => {
                                    if (
                                        suppressClickTabId.current === tab.id
                                    ) {
                                        event.preventDefault();
                                        suppressClickTabId.current = null;
                                        return;
                                    }
                                    if (event.detail === 0) {
                                        onSelect(tab.id);
                                    }
                                }}
                                role="tab"
                                type="button"
                            >
                                <DeviceTabLabel device={device} />
                            </button>
                            <button
                                aria-label={`关闭标签页：${device?.name ?? "新标签页"}`}
                                className="device-tab-close"
                                disabled={pending}
                                draggable={false}
                                onClick={(event) => {
                                    event.stopPropagation();
                                    onClose(tab.id);
                                }}
                                onPointerDown={(event) =>
                                    event.stopPropagation()
                                }
                                title="关闭标签页"
                                type="button"
                            >
                                <X aria-hidden="true" size={15} />
                            </button>
                        </div>
                    );
                })}
                <button
                    aria-label="新建标签页"
                    className="device-new-tab-button"
                    onClick={onNewTab}
                    title="新建标签页"
                    type="button"
                >
                    <Plus aria-hidden="true" size={19} />
                </button>
            </div>
            {dragPreview &&
                createPortal(
                    <div
                        aria-hidden="true"
                        className="device-tab-item device-tab-drag-preview"
                        style={{
                            height: dragPreview.height,
                            transform: `translate3d(${dragPreview.x}px, ${dragPreview.y}px, 0)`,
                            width: dragPreview.width,
                        }}
                    >
                        <div className="device-tab-select">
                            <DeviceTabLabel device={previewDevice} />
                        </div>
                        <span className="device-tab-close">
                            <X aria-hidden="true" size={15} />
                        </span>
                    </div>,
                    document.body,
                )}
        </div>
    );
};
