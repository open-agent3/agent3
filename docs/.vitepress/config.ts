import { defineConfig } from 'vitepress'

export default defineConfig({
  title: "Agent3",
  description: "The system-level ambient voice agent. Inspired by Her.",
  appearance: "dark",
  base: "/agent3/",
  
  themeConfig: {
    nav: [
      { text: 'Home', link: '/' },
      { text: 'Architecture', link: '/ARCHITECTURE' }
    ],

    sidebar: [
      {
        text: 'Documentation',
        items: [
          { text: 'Architecture', link: '/ARCHITECTURE' }
        ]
      }
    ],

    socialLinks: [
      { icon: 'github', link: 'https://github.com/open-agent3/agent3' } // Replace with actual URL
    ]
  }
})
