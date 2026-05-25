import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, fireEvent, waitFor } from '@testing-library/react'
import React from 'react'

type TxListener = (e: { payload: { step: string; status: string; detail?: string } }) => void

let txListener: TxListener | null = null

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn().mockImplementation((event: string, cb: TxListener) => {
    if (event === 'tx-progress') txListener = cb
    return Promise.resolve(() => {
      txListener = null
    })
  }),
}))

import OrganizeProgress from '../components/OrganizeProgress'

describe('OrganizeProgress', () => {
  beforeEach(() => {
    txListener = null
  })

  it('is hidden until a transaction starts', async () => {
    render(<OrganizeProgress />)
    expect(screen.queryByText('논문 정리 중')).not.toBeInTheDocument()
  })

  it('shows progress when tx-progress events arrive', async () => {
    render(<OrganizeProgress />)
    await waitFor(() => expect(txListener).not.toBeNull())

    txListener!({
      payload: { step: 'MovedToStaging', status: 'started' },
    })
    txListener!({
      payload: { step: 'MovedToStaging', status: 'done' },
    })
    txListener!({
      payload: { step: 'GeminiClassified', status: 'done', detail: 'nlp' },
    })

    await waitFor(() => {
      expect(screen.getByText('논문 정리 중')).toBeInTheDocument()
      expect(screen.getByText('스테이징으로 이동')).toBeInTheDocument()
      expect(screen.getByText('AI 분류')).toBeInTheDocument()
    })
  })

  it('can be dismissed after all steps complete', async () => {
    render(<OrganizeProgress />)
    await waitFor(() => expect(txListener).not.toBeNull())

    for (const step of [
      'MovedToStaging',
      'GeminiClassified',
      'MovedToTarget',
      'ZoteroCollectionChanged',
      'ZotMovConfirmed',
    ]) {
      if (step === 'MovedToStaging') {
        txListener!({ payload: { step, status: 'started' } })
      }
      txListener!({ payload: { step, status: 'done' } })
    }

    await waitFor(() => {
      expect(screen.getByRole('button', { name: '닫기' })).toBeInTheDocument()
    })

    fireEvent.click(screen.getByRole('button', { name: '닫기' }))
    await waitFor(() => {
      expect(screen.queryByText('논문 정리 중')).not.toBeInTheDocument()
    })
  })
})
