import { act, render, screen } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { LiveOutput } from './LiveOutput'

/** A fake `WebSocket` recording the URL it was opened with and letting a test drive its
 * lifecycle by hand — jsdom has no real WebSocket server to connect to. */
class FakeWebSocket {
  static instances: FakeWebSocket[] = []
  url: string
  onopen: (() => void) | null = null
  onmessage: ((event: { data: string }) => void) | null = null
  onclose: (() => void) | null = null
  closed = false

  constructor(url: string) {
    this.url = url
    FakeWebSocket.instances.push(this)
  }

  close() {
    this.closed = true
    this.onclose?.()
  }
}

afterEach(() => {
  vi.unstubAllGlobals()
  FakeWebSocket.instances = []
})

describe('LiveOutput', () => {
  it('opens a websocket at the sessions live endpoint and shows a waiting state first', () => {
    vi.stubGlobal('WebSocket', FakeWebSocket)

    render(<LiveOutput sessionId="s-1" />)

    expect(screen.getByText('Waiting for output…')).toBeInTheDocument()
    expect(FakeWebSocket.instances).toHaveLength(1)
    expect(FakeWebSocket.instances[0]!.url).toContain('/api/v1/agent-sessions/s-1/live')
    expect(FakeWebSocket.instances[0]!.url.startsWith('ws://')).toBe(true)
  })

  it('renders each incoming line, summarised, as it arrives', async () => {
    vi.stubGlobal('WebSocket', FakeWebSocket)
    render(<LiveOutput sessionId="s-1" />)
    const socket = FakeWebSocket.instances[0]!

    act(() => {
      socket.onopen?.()
      socket.onmessage?.({
        data: JSON.stringify({ type: 'system', subtype: 'init', model: 'claude-opus-5' }),
      })
    })
    expect(await screen.findByText('Session started (claude-opus-5)')).toBeInTheDocument()

    act(() => {
      socket.onmessage?.({
        data: JSON.stringify({
          type: 'assistant',
          message: { content: [{ type: 'text', text: 'Reading the file.' }] },
        }),
      })
    })
    expect(await screen.findByText('Reading the file.')).toBeInTheDocument()
    // The first line is still there — this is an accumulating log, not a single "latest".
    expect(screen.getByText('Session started (claude-opus-5)')).toBeInTheDocument()
  })

  it('closes the socket on unmount', () => {
    vi.stubGlobal('WebSocket', FakeWebSocket)
    const { unmount } = render(<LiveOutput sessionId="s-1" />)
    const socket = FakeWebSocket.instances[0]!
    act(() => socket.onopen?.())

    unmount()

    expect(socket.closed).toBe(true)
  })
})
