<script setup lang="ts">
import { ref, onMounted, onUnmounted } from 'vue';
import DownloadButton from './DownloadButton.vue';

// Simulate an audio energy value for the standalone landing page
const audioEnergy = ref(0);
let animationFrameId: number;

onMounted(() => {
  let time = 0;
  
  const animate = () => {
    time += 0.05;
    // Simulate a breathing / listening pattern
    // Mix low frequency for breathing and high frequency for "speaking" bursts
    const baseBreath = (Math.sin(time) * 0.5 + 0.5) * 0.3; // 0.0 to 0.3
    const voiceBurst = (Math.sin(time * 3.5) * Math.cos(time * 1.8) > 0.5) 
                        ? Math.random() * 0.5 : 0;
    
    // Smooth the energy slightly
    const targetEnergy = baseBreath + voiceBurst;
    audioEnergy.value += (targetEnergy - audioEnergy.value) * 0.1;
    
    // Pass it to the CSS variable
    document.documentElement.style.setProperty('--pseudo-energy', audioEnergy.value.toString());
    
    animationFrameId = requestAnimationFrame(animate);
  };
  
  animate();
});

onUnmounted(() => {
  cancelAnimationFrame(animationFrameId);
  document.documentElement.style.removeProperty('--pseudo-energy');
});
</script>

<template>
  <div class="hero home-page-env">
    <div class="glow-container">
      <div class="glow-orb orb-1"></div>
      <div class="glow-orb orb-2"></div>
      <div class="glow-orb orb-3"></div>
    </div>
    
    <div class="content">
      <h1 class="title">Agent3</h1>
      <p class="tagline">System-level ambient AI voice agent.</p>
      <p class="inspired">Inspired by the movie <em>Her</em>.</p>
      
      <DownloadButton />
    </div>
  </div>
</template>

<style scoped>
.hero {
  position: relative;
  width: 100%;
  height: 100vh;
  /* Adjust for default Vitepress padding/top margin if they bleed through */
  margin-top: calc(-1 * var(--vp-nav-height, 0px));
  display: flex;
  align-items: center;
  justify-content: center;
  overflow: hidden;
  background-color: transparent;
}

/* Base setup for the edge glow/audio effect taken from App.vue concepts */
.glow-container {
  position: absolute;
  top: 50%;
  left: 50%;
  width: 100%;
  height: 100%;
  transform: translate(-50%, -50%);
  pointer-events: none;
  z-index: 0;
  display: flex;
  justify-content: center;
  align-items: center;
  overflow: hidden;
}

.glow-orb {
  position: absolute;
  border-radius: 50%;
  filter: blur(80px);
  opacity: calc(0.5 + var(--pseudo-energy, 0) * 0.5);
  transition: transform 0.1s linear, opacity 0.1s linear;
}

.orb-1 {
  width: 50vw;
  height: 50vw;
  background: rgba(255, 81, 47, 0.4);
  transform: translate(-15%, -15%) scale(calc(0.8 + var(--pseudo-energy, 0) * 0.4));
}

.orb-2 {
  width: 45vw;
  height: 45vw;
  background: rgba(221, 36, 118, 0.4);
  transform: translate(15%, 15%) scale(calc(0.9 + var(--pseudo-energy, 0) * 0.5));
}

.orb-3 {
  width: 60vw;
  height: 60vw;
  background: rgba(255, 105, 53, 0.25);
  transform: translate(0, 0) scale(calc(0.7 + var(--pseudo-energy, 0) * 0.6));
}

.content {
  position: relative;
  z-index: 10;
  text-align: center;
  display: flex;
  flex-direction: column;
  align-items: center;
  padding: 0 24px;
}

.title {
  font-size: clamp(3rem, 8vw, 6rem);
  font-weight: 800;
  letter-spacing: -0.04em;
  line-height: 1;
  background: linear-gradient(180deg, #ffffff 0%, rgba(255, 255, 255, 0.5) 100%);
  -webkit-background-clip: text;
  -webkit-text-fill-color: transparent;
  margin-bottom: 24px;
}

.tagline {
  font-size: clamp(1.2rem, 3vw, 1.8rem);
  color: var(--vp-c-text-2);
  margin-bottom: 8px;
  font-weight: 400;
}

.inspired {
  font-size: 1rem;
  color: var(--vp-c-text-3);
  font-weight: 300;
  letter-spacing: 2px;
  text-transform: uppercase;
  opacity: 0.6;
}
</style>
