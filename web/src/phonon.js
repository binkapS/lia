import { PhononTTS, DEFAULT_MODEL, clearCache } from 'phonon-tts';

if (typeof window !== 'undefined') {
  window.PhononTTS = PhononTTS;
  window.PhononDEFAULT_MODEL = DEFAULT_MODEL;
  window.phononClearCache = clearCache;
}

export { PhononTTS, DEFAULT_MODEL, clearCache };
