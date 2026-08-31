// @vitest-environment jsdom
import { describe, it, expect } from 'vitest'
import { applyLayoutWidth, LAYOUT_WIDTHS } from './layout'

describe('applyLayoutWidth', () => {
  // The widths live in CSS keyed off this attribute, so the value written has
  // to be exactly one the stylesheet selects on — a typo here silently leaves
  // the app at its default width with nothing to explain why.
  it('writes each width as a root data attribute', () => {
    for (const [value] of LAYOUT_WIDTHS) {
      applyLayoutWidth(value)
      expect(document.documentElement.dataset.layoutWidth).toBe(value)
    }
  })
})
