'use client'

import React, { createContext, useContext, useState, useCallback } from 'react'
import { type PaperMeta } from '@/lib/content'
import PaperPreviewDrawer from './PaperPreviewDrawer'

interface PaperPreviewContextType {
  previewPaper: PaperMeta | null
  openPreview: (paper: PaperMeta) => void
  closePreview: () => void
}

const PaperPreviewContext = createContext<PaperPreviewContextType | undefined>(undefined)

export function PaperPreviewProvider({ children }: { children: React.ReactNode }) {
  const [previewPaper, setPreviewPaper] = useState<PaperMeta | null>(null)

  const openPreview = useCallback((paper: PaperMeta) => {
    setPreviewPaper(paper)
  }, [])

  const closePreview = useCallback(() => {
    setPreviewPaper(null)
  }, [])

  return (
    <PaperPreviewContext.Provider value={{ previewPaper, openPreview, closePreview }}>
      {children}
      <PaperPreviewDrawer />
    </PaperPreviewContext.Provider>
  )
}

export function usePaperPreview() {
  const context = useContext(PaperPreviewContext)
  if (!context) {
    throw new Error('usePaperPreview must be used within a PaperPreviewProvider')
  }
  return context
}
