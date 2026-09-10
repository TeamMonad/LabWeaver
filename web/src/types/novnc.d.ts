declare module '@novnc/novnc/lib/rfb' {
  interface RfbOptions {
    wsProtocols?: string[]
    [key: string]: unknown
  }

  export default class RFB {
    constructor(target: HTMLElement, url: string, options?: RfbOptions)
    addEventListener(type: string, listener: (event: Event) => void): void
    disconnect(): void
  }
}
