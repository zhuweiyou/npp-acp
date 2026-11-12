// Thin bridge over the wry-injected `window.ipc.postMessage` and the
// `window.__nppacp.emit` callback that the Rust side invokes via evaluate_script.

export type IpcMessage = { type: string; [key: string]: unknown }

type EmitFn = (event: IpcMessage) => void

declare global {
  interface Window {
    ipc?: { postMessage: (data: string) => void }
    __nppacp?: { emit: EmitFn }
  }
}

export function hasTransport(): boolean {
  return typeof window !== "undefined" && !!window.ipc?.postMessage
}

export function send(message: IpcMessage): boolean {
  if (!hasTransport()) {
    return false
  }
  window.ipc!.postMessage(JSON.stringify(message))
  return true
}

// Register the receiver that Rust calls as `window.__nppacp.emit({...})`.
export function onEvent(handler: EmitFn): () => void {
  window.__nppacp = { emit: handler }
  return () => {
    if (window.__nppacp?.emit === handler) {
      delete window.__nppacp
    }
  }
}
