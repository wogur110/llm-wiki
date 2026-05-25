// Extend vitest's `expect` with @testing-library/jest-dom custom matchers
// (e.g. toBeInTheDocument, toBeDisabled, toHaveTextContent, …)
import '@testing-library/jest-dom/vitest'

import { afterEach } from 'vitest'
import { cleanup } from '@testing-library/react'

// @testing-library/react relies on a global `afterEach` to unmount components
// between tests. Vitest does not register globals unless `globals: true` is set
// in the config, so we wire cleanup up explicitly here.
afterEach(() => {
  cleanup()
})
