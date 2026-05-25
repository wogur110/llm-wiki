import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen } from '@testing-library/react'
import React from 'react'

let mockPathname = '/'

vi.mock('next/navigation', () => ({
  usePathname: () => mockPathname,
  useRouter: () => ({ push: vi.fn() }),
}))

vi.mock('../components/SearchBar', () => ({
  default: () => <input data-testid="search-bar" placeholder="논문 검색…" />,
}))

import Header from '../components/Header'

describe('Header', () => {
  beforeEach(() => {
    mockPathname = '/'
  })

  it('renders logo and nav links on normal pages', () => {
    render(<Header />)
    expect(screen.getByText('LLM Wiki')).toBeInTheDocument()
    expect(screen.getByText('대시보드')).toBeInTheDocument()
    expect(screen.getByText('설정')).toBeInTheDocument()
    expect(screen.getByTestId('search-bar')).toBeInTheDocument()
  })

  it('returns null on /onboarding', () => {
    mockPathname = '/onboarding'
    const { container } = render(<Header />)
    expect(container.firstChild).toBeNull()
  })
})
