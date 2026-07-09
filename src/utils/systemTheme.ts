import { useEffect, useRef, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import type { ThemeInfo } from '../types/clipboard'

/**
 * Query the backend for the current system color scheme.
 * The backend reads the macOS appearance via Tauri's built-in theme.
 */
export async function getSystemThemeFromPortal(): Promise<boolean | null> {
  try {
    const themeInfo = await invoke<ThemeInfo>('get_system_theme')
    if (themeInfo.source !== 'default') {
      return themeInfo.prefers_dark
    }
    return null
  } catch (error) {
    console.warn('[systemTheme] Failed to get system theme from portal:', error)
    return null
  }
}

/**
 * Hook for detecting system dark mode preference.
 * Uses the CSS media query as the primary source, backed by the macOS appearance
 * reported by the backend. Live changes arrive via the `system-theme-changed`
 * event (emitted from the macOS appearance-changed notification); a polling
 * fallback only runs if that event stream is ever reported inactive.
 */
export function useSystemThemePreference(): boolean {
  const [systemPrefersDark, setSystemPrefersDark] = useState(() => {
    if (globalThis.matchMedia) {
      return globalThis.matchMedia('(prefers-color-scheme: dark)').matches
    }
    return true
  })
  const hasCheckedPortal = useRef(false)

  // Check the backend for the initial macOS appearance
  useEffect(() => {
    if (hasCheckedPortal.current) return
    hasCheckedPortal.current = true

    getSystemThemeFromPortal().then((portalPrefersDark) => {
      if (portalPrefersDark !== null) {
        setSystemPrefersDark(portalPrefersDark)
      }
    })
  }, [])

  // Listen for media query changes
  useEffect(() => {
    const mediaQuery = globalThis.matchMedia('(prefers-color-scheme: dark)')
    const handleChange = (e: MediaQueryListEvent) => {
      setSystemPrefersDark(e.matches)
    }
    mediaQuery.addEventListener('change', handleChange)
    return () => mediaQuery.removeEventListener('change', handleChange)
  }, [])

  // Listen for theme change events from the backend (macOS appearance changes)
  useEffect(() => {
    const unlistenPromise = listen<ThemeInfo>('system-theme-changed', (event) => {
      const themeInfo = event.payload
      setSystemPrefersDark(themeInfo.prefers_dark)
    })

    return () => {
      unlistenPromise.then((unlisten) => unlisten())
    }
  }, [])

  // Polling fallback: only poll if the backend reports the theme-change event
  // stream as inactive (on macOS it is always active, so this never runs)
  useEffect(() => {
    let checkInterval: number | null = null

    const setupPolling = async () => {
      const hasEventListener = await invoke<boolean>('is_theme_listener_active')

      if (!hasEventListener) {
        // Event listener not available, use polling fallback
        checkInterval = window.setInterval(async () => {
          const portalPrefersDark = await getSystemThemeFromPortal()
          if (portalPrefersDark !== null) {
            setSystemPrefersDark(portalPrefersDark)
          }
        }, 10000) // Check every 10 seconds
      }
    }

    setupPolling()

    return () => {
      if (checkInterval) clearInterval(checkInterval)
    }
  }, [])

  return systemPrefersDark
}
