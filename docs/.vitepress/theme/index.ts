import DefaultTheme from 'vitepress/theme'
import './custom.css'
import HeroHome from '../components/HeroHome.vue'
import DownloadButton from '../components/DownloadButton.vue'
import type { Theme } from 'vitepress'

export default {
  extends: DefaultTheme,
  enhanceApp({ app }) {
    app.component('HeroHome', HeroHome)
    app.component('DownloadButton', DownloadButton)
  }
} satisfies Theme