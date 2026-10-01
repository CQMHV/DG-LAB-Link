import { useCallback, useEffect, useRef, useState } from "react";

import type { HubSnapshot } from "../lib/contracts";
import { getHubSnapshot, listenHubSnapshot, listenRuntimeError } from "../lib/bridge";
import { getErrorMessage } from "../lib/errors";

interface HubSnapshotState {
    snapshot: HubSnapshot | null;
    loading: boolean;
    error: string | null;
    refresh: () => Promise<void>;
}

export const useHubSnapshot = (): HubSnapshotState => {
    const [snapshot, setSnapshot] = useState<HubSnapshot | null>(null);
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);
    const coreDisconnected = useRef(false);

    const acceptSnapshot = useCallback((next: HubSnapshot) => {
        if (coreDisconnected.current) {
            return;
        }
        setSnapshot((current) => {
            if (current && next.revision <= current.revision) {
                return current;
            }
            return next;
        });
        setError(null);
    }, []);

    const refresh = useCallback(async () => {
        try {
            const next = await getHubSnapshot();
            acceptSnapshot(next);
        } catch (refreshError) {
            setError(getErrorMessage(refreshError, "无法读取中枢状态"));
        }
    }, [acceptSnapshot]);

    useEffect(() => {
        let active = true;
        let unlisten: (() => void) | null = null;
        let unlistenError: (() => void) | null = null;

        const initialise = async () => {
            try {
                const stopListening = await listenHubSnapshot((next) => {
                    if (active) {
                        acceptSnapshot(next);
                    }
                });

                if (!active) {
                    stopListening();
                    return;
                }

                unlisten = stopListening;
                const stopListeningError = await listenRuntimeError((message) => {
                    if (active) {
                        coreDisconnected.current = true;
                        setError(message);
                        setSnapshot(null);
                    }
                });
                if (!active) {
                    stopListeningError();
                    return;
                }
                unlistenError = stopListeningError;
                const initial = await getHubSnapshot();
                if (active) {
                    acceptSnapshot(initial);
                    setLoading(false);
                }
            } catch (initialiseError) {
                if (active) {
                    setError(
                        getErrorMessage(initialiseError, "无法读取中枢状态"),
                    );
                    setLoading(false);
                }
                const stopListening = unlisten;
                unlisten = null;
                stopListening?.();
                unlistenError?.();
                unlistenError = null;
            }
        };

        void initialise();

        return () => {
            active = false;
            unlisten?.();
            unlistenError?.();
        };
    }, [acceptSnapshot]);

    return { snapshot, loading, error, refresh };
};
