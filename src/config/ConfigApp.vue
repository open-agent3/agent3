<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, nextTick } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { emit, listen, type UnlistenFn } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";
import { t, initLocale, setLocale, locale, type Locale } from "../i18n";

// ---- Type Definitions ----
type ProviderType = "openai" | "gemini";

interface Provider {
  id: string;
  name: string;
  base_url: string;
  api_key: string;
  model: string;
  is_active: boolean;
  provider_type: ProviderType;
}

interface WakewordInfo {
  name: string;
  path: string;
}

// ---- Presets ----
const SENSORY_PRESETS: Record<string, { name: string; base_url: string; model: string; provider_type: ProviderType }> = {
  openai: {
    name: "OpenAI Realtime",
    base_url: "wss://api.openai.com/v1/realtime",
    model: "gpt-4o-realtime-preview-2024-12-17",
    provider_type: "openai",
  },
  gemini: {
    name: "Gemini Multimodal Live",
    base_url: "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent",
    model: "gemini-2.5-flash-native-audio-preview-12-2025",
    provider_type: "gemini",
  },
};



// ---- Voice Constants ----
const GEMINI_VOICES: { name: string; descKey: string }[] = [
  { name: "Aoede", descKey: "voices.gemini.Aoede" },
  { name: "Puck", descKey: "voices.gemini.Puck" },
  { name: "Kore", descKey: "voices.gemini.Kore" },
  { name: "Charon", descKey: "voices.gemini.Charon" },
  { name: "Fenrir", descKey: "voices.gemini.Fenrir" },
  { name: "Leda", descKey: "voices.gemini.Leda" },
  { name: "Orus", descKey: "voices.gemini.Orus" },
  { name: "Zephyr", descKey: "voices.gemini.Zephyr" },
  { name: "Callirrhoe", descKey: "voices.gemini.Callirrhoe" },
  { name: "Autonoe", descKey: "voices.gemini.Autonoe" },
  { name: "Enceladus", descKey: "voices.gemini.Enceladus" },
  { name: "Iapetus", descKey: "voices.gemini.Iapetus" },
  { name: "Umbriel", descKey: "voices.gemini.Umbriel" },
  { name: "Algieba", descKey: "voices.gemini.Algieba" },
  { name: "Despina", descKey: "voices.gemini.Despina" },
  { name: "Erinome", descKey: "voices.gemini.Erinome" },
  { name: "Algenib", descKey: "voices.gemini.Algenib" },
  { name: "Rasalgethi", descKey: "voices.gemini.Rasalgethi" },
  { name: "Laomedeia", descKey: "voices.gemini.Laomedeia" },
  { name: "Achernar", descKey: "voices.gemini.Achernar" },
  { name: "Alnilam", descKey: "voices.gemini.Alnilam" },
  { name: "Schedar", descKey: "voices.gemini.Schedar" },
  { name: "Gacrux", descKey: "voices.gemini.Gacrux" },
  { name: "Pulcherrima", descKey: "voices.gemini.Pulcherrima" },
  { name: "Achird", descKey: "voices.gemini.Achird" },
  { name: "Zubenelgenubi", descKey: "voices.gemini.Zubenelgenubi" },
  { name: "Vindemiatrix", descKey: "voices.gemini.Vindemiatrix" },
  { name: "Sadachbia", descKey: "voices.gemini.Sadachbia" },
  { name: "Sadaltager", descKey: "voices.gemini.Sadaltager" },
  { name: "Sulafat", descKey: "voices.gemini.Sulafat" },
];

const OPENAI_VOICES: { name: string; descKey: string }[] = [
  { name: "alloy", descKey: "voices.openai.alloy" },
  { name: "ash", descKey: "voices.openai.ash" },
  { name: "ballad", descKey: "voices.openai.ballad" },
  { name: "coral", descKey: "voices.openai.coral" },
  { name: "echo", descKey: "voices.openai.echo" },
  { name: "sage", descKey: "voices.openai.sage" },
  { name: "shimmer", descKey: "voices.openai.shimmer" },
  { name: "verse", descKey: "voices.openai.verse" },
];

// ---- State ----
const activeTab = ref<"general" | "sensory">("general");
const providers = ref<Provider[]>([]);
const agentName = ref("");
const statusMsg = ref("");
const selectedVoice = ref("");
// ---- Wizard State ----
type WizardStage = "provider" | "apikey" | "wakeword" | "done";
const wizardStage = ref<WizardStage>("provider");
const isFirstRun = computed(() => providers.value.length === 0);
const showWizard = computed(() => isFirstRun.value || wizardStage.value === "wakeword");
const wizardProviderType = ref<ProviderType | null>(null);
const wizardApiKey = ref("");
const wizardLoading = ref(false);
const wizardWakewordExpanded = ref(false);

const voiceSwitching = ref(false);
const showSensoryForm = ref(false);

// ---- Autostart State ----
const autostartEnabled = ref(true);

// ---- Wakeword State ----
const wakewordEnabled = ref(false);
const wakewordModels = ref<WakewordInfo[]>([]);
const wakewordActiveModel = ref("");
const wakewordRecording = ref(false);
const wakewordSampleCount = ref(0);
const wakewordTrainName = ref("");
const wakewordTraining = ref(false);
const wakewordLastDuration = ref<number | null>(null);
const wakewordLastQualityHint = ref("");
const wakewordSetupSkipped = ref(false);
const wakewordSectionEl = ref<HTMLElement | null>(null);
let unlistenFocusWakeword: UnlistenFn | null = null;

const wakewordProgress = computed(() => {
  const current = Math.min(wakewordSampleCount.value, 3);
  return Math.round((current / 3) * 100);
});

// ---- Forms ----
const sensoryForm = ref({ preset: "openai", name: "", base_url: "", api_key: "", model: "", provider_type: "openai" as ProviderType });

function applySensoryPreset() {
  const p = SENSORY_PRESETS[sensoryForm.value.preset];
  if (p) {
    sensoryForm.value.name = p.name;
    sensoryForm.value.base_url = p.base_url;
    sensoryForm.value.model = p.model;
    sensoryForm.value.provider_type = p.provider_type;
  }
}

// ---- Computed Properties ----
const realtimeProviders = computed(() => providers.value);

// ---- Operations ----
async function loadProviders() {
  try {
    providers.value = await invoke("get_providers");
  } catch {
    // Rust already logged the error, frontend keeps empty list
  }
}

async function loadAgentName() {
  try {
    const val = await invoke<string | null>("get_setting", { key: "agent_name" });
    agentName.value = val || "";
  } catch {
    // Rust already logged the error
  }
}

async function saveAgentName() {
  try {
    await invoke("set_setting", { key: "agent_name", value: agentName.value });
    showStatus(t("config.general_name_saved"));
    await emit("config-changed");
  } catch (e) {
    showStatus(`${t("config.general_save_fail_prefix")}${e}`);
  }
}

async function addProvider() {
  const form = sensoryForm.value;

  const provider: Provider = {
    id: `provider_${Date.now()}`,
    name: form.name,
    base_url: form.base_url,
    api_key: form.api_key,
    model: form.model,
    provider_type: form.provider_type,
    is_active: realtimeProviders.value.length === 0,
  };

  try {
    await invoke("save_provider", { provider });
    await loadProviders();
    sensoryForm.value = { preset: "openai", name: "", base_url: "", api_key: "", model: "", provider_type: "openai" };
    applySensoryPreset();
    showStatus(t("config.sensory_added"));
    showSensoryForm.value = false;
    await emit("config-changed");
  } catch (e) {
    showStatus(`${t("config.sensory_add_fail_prefix")}${e}`);
  }
}

async function removeProvider(id: string) {
  try {
    await invoke("delete_provider", { id });
    await loadProviders();
    showStatus(t("config.sensory_removed"));
    await emit("config-changed");
  } catch (e) {
    showStatus(`${t("config.sensory_remove_fail_prefix")}${e}`);
  }
}

async function activateProvider(id: string) {
  try {
    await invoke("set_active_provider", { id });
    await loadProviders();
    showStatus(t("config.sensory_activated"));
    await emit("config-changed");
  } catch (e) {
    showStatus(`${t("config.sensory_activate_fail_prefix")}${e}`);
  }
}

// ---- Voice Operations ----
const activeVoiceList = computed(() => {
  const active = realtimeProviders.value.find((p) => p.is_active);
  if (!active) return GEMINI_VOICES;
  return active.provider_type === "openai" ? OPENAI_VOICES : GEMINI_VOICES;
});

async function loadVoice() {
  try {
    const val = await invoke<string | null>("get_setting", { key: "voice_name" });
    selectedVoice.value = val || "";
  } catch {
    // Rust already logged the error
  }
}

async function switchVoice(voiceName: string) {
  selectedVoice.value = voiceName;
  voiceSwitching.value = true;
  try {
    await invoke("agent_switch_voice", { voice: voiceName });
    showStatus(`${t("config.voice_switch_prefix")}${voiceName}`);
  } catch (e) {
    showStatus(`${t("config.voice_switch_fail_prefix")}${e}`);
  } finally {
    voiceSwitching.value = false;
  }
}

// ---- First-run Wizard Operations ----
async function openApiKeyPage() {
  if (!wizardProviderType.value) return;
  let url = "";
  if (wizardProviderType.value === "openai") url = "https://platform.openai.com/api-keys";
  else if (wizardProviderType.value === "gemini") url = "https://aistudio.google.com/app/apikey";
  
  if (url) {
    try {
      await openUrl(url);
    } catch(e) {
      console.error("Failed to open URL", e);
    }
  }
}

async function quickConnect() {
  if (!wizardApiKey.value.trim() || !wizardProviderType.value) {
    showStatus(t("config.wizard_apikey_required"));
    return;
  }
  
  wizardLoading.value = true;
  try {
    const pType = wizardProviderType.value;
    
    // Configure sensory layer (Realtime WS)
    if (SENSORY_PRESETS[pType]) {
      const sp = SENSORY_PRESETS[pType];
      const sensoryProvider: Provider = {
        id: `provider_s_${Date.now()}`,
        name: sp.name,
        base_url: sp.base_url,
        api_key: wizardApiKey.value.trim(),
        model: sp.model,
        provider_type: sp.provider_type,
        is_active: true,
      };
      await invoke("save_provider", { provider: sensoryProvider });
    }
    
    await loadProviders();
    showStatus(t("config.wizard_success"));
    await emit("config-changed");
    wizardStage.value = "wakeword";
  } catch (e) {
    showStatus(`${t("config.wizard_fail_prefix")}${e}`);
  } finally {
    wizardLoading.value = false;
  }
}

function finishWizard(skipWakeword: boolean) {
  wizardStage.value = "done";
  wizardWakewordExpanded.value = false;
  if (skipWakeword) {
    markWakewordSkipState(true);
    showStatus(t("config.wizard_wakeword_skipped"));
  } else {
    markWakewordSkipState(false);
    showStatus(t("config.wizard_wakeword_done"));
  }
}

function showStatus(msg: string) {
  statusMsg.value = msg;
  setTimeout(() => (statusMsg.value = ""), 2500);
}

// ---- Wakeword Operations ----
async function loadWakewordSettings() {
  try {
    const enabled = await invoke<string | null>("get_setting", { key: "wake_word_enabled" });
    wakewordEnabled.value = enabled === "true";
    const modelPath = await invoke<string | null>("get_setting", { key: "wake_word_model_path" });
    wakewordActiveModel.value = modelPath || "";
    const skipped = await invoke<string | null>("get_setting", { key: "wakeword_setup_skipped" });
    wakewordSetupSkipped.value = skipped === "true";
    wakewordModels.value = await invoke<WakewordInfo[]>("wakeword_list");
  } catch {
    // Rust already logged the error
  }
}

async function markWakewordSkipState(skipped: boolean) {
  try {
    await invoke("set_setting", {
      key: "wakeword_setup_skipped",
      value: skipped ? "true" : "false",
    });
    wakewordSetupSkipped.value = skipped;
  } catch {
    // Rust already logged the error
  }
}

async function toggleWakeword() {
  try {
    await invoke("set_setting", { key: "wake_word_enabled", value: wakewordEnabled.value ? "true" : "false" });
    showStatus(wakewordEnabled.value ? t("config.wakeword_enabled_toast") : t("config.wakeword_disabled_toast"));
    await emit("config-changed");
  } catch (e) {
    showStatus(`${t("config.general_save_fail_prefix")}${e}`);
  }
}

async function startRecording() {
  try {
    await invoke("wakeword_start_record");
    wakewordRecording.value = true;
    showStatus(t("config.wakeword_recording_toast"));
  } catch (e) {
    showStatus(`${t("config.general_save_fail_prefix")}${e}`);
  }
}

async function stopRecording() {
  try {
    const duration = await invoke<number>("wakeword_stop_record");
    wakewordRecording.value = false;
    wakewordLastDuration.value = duration;
    if (duration < 0.3) {
      wakewordLastQualityHint.value = t("config.wakeword_record_quality_short");
      showStatus(t("config.wakeword_record_too_short"));
      return;
    }

    if (duration < 0.8) {
      wakewordLastQualityHint.value = t("config.wakeword_record_quality_short");
    } else if (duration > 3.0) {
      wakewordLastQualityHint.value = t("config.wakeword_record_quality_long");
    } else {
      wakewordLastQualityHint.value = t("config.wakeword_record_quality_good");
    }

    await invoke("wakeword_save_sample", { index: wakewordSampleCount.value });
    wakewordSampleCount.value++;
    showStatus(`${t("config.wakeword_sample_saved_prefix")}${wakewordSampleCount.value} (${duration.toFixed(1)}s)`);
  } catch (e) {
    wakewordRecording.value = false;
    showStatus(`${t("config.general_save_fail_prefix")}${e}`);
  }
}

async function trainModel() {
  const name = wakewordTrainName.value.trim();
  if (!name) {
    showStatus(t("config.wakeword_name_required"));
    return;
  }
  if (wakewordSampleCount.value < 3) {
    showStatus(t("config.wakeword_samples_required"));
    return;
  }
  wakewordTraining.value = true;
  try {
    const modelPath = await invoke<string>("wakeword_train", { name });
    showStatus(`${t("config.wakeword_train_success")} ${t("config.wakeword_test_tip")}`);
    wakewordSampleCount.value = 0;
    wakewordTrainName.value = "";
    wakewordLastDuration.value = null;
    wakewordLastQualityHint.value = "";
    // Auto-activate new model
    await invoke("wakeword_set_active", { modelPath, enabled: true });
    wakewordEnabled.value = true;
    wakewordActiveModel.value = modelPath;
    await markWakewordSkipState(false);
    await loadWakewordSettings();
    await emit("config-changed");
  } catch (e) {
    showStatus(`${t("config.wakeword_train_fail_prefix")}${e}`);
  } finally {
    wakewordTraining.value = false;
  }
}

async function activateModel(model: WakewordInfo) {
  try {
    await invoke("wakeword_set_active", { modelPath: model.path, enabled: true });
    wakewordEnabled.value = true;
    wakewordActiveModel.value = model.path;
    await markWakewordSkipState(false);
    showStatus(`${t("config.wakeword_activated_prefix")}${model.name}`);
    await emit("config-changed");
  } catch (e) {
    showStatus(`${t("config.wakeword_activate_fail_prefix")}${e}`);
  }
}

async function deleteModel(model: WakewordInfo) {
  try {
    await invoke("wakeword_delete", { name: model.name });
    let deletedActive = false;
    if (wakewordActiveModel.value === model.path) {
      deletedActive = true;
      wakewordActiveModel.value = "";
      await invoke("set_setting", { key: "wake_word_model_path", value: "" });
      await invoke("set_setting", { key: "wake_word_enabled", value: "false" });
      wakewordEnabled.value = false;
      await markWakewordSkipState(true);
    }
    await loadWakewordSettings();
    if (deletedActive) {
      showStatus(`${t("config.wakeword_deleted_and_disabled_prefix")}${model.name}`);
    } else {
      showStatus(`${t("config.wakeword_deleted_prefix")}${model.name}`);
    }
    await emit("config-changed");
  } catch (e) {
    showStatus(`${t("config.wakeword_delete_fail_prefix")}${e}`);
  }
}

async function loadAutostart() {
  try {
    autostartEnabled.value = await invoke<boolean>("autostart_is_enabled");
  } catch {
    // Keep current UI state when backend query fails
  }
}

async function toggleAutostart() {
  try {
    await invoke("autostart_set_enabled", { enabled: autostartEnabled.value });
    await invoke("set_setting", { key: "autostart_enabled", value: autostartEnabled.value ? "true" : "false" });
    showStatus(autostartEnabled.value ? t("config.autostart_enabled_toast") : t("config.autostart_disabled_toast"));
  } catch (e) {
    showStatus(`${t("config.general_save_fail_prefix")}${e}`);
  }
}

async function changeLanguage(lang: Locale) {
  await setLocale(lang);
  await emit("config-changed");
}

onMounted(() => {
  initLocale();
  loadProviders();
  loadAgentName();
  loadWakewordSettings();
  loadAutostart();
  loadVoice();
  applySensoryPreset();
  wizardStage.value = "provider";

  listen("config-focus-wakeword", async () => {
    activeTab.value = "general";
    await nextTick();
    wakewordSectionEl.value?.scrollIntoView({ behavior: "smooth", block: "center" });
  }).then((u) => {
    unlistenFocusWakeword = u;
  });
});

onUnmounted(() => {
  unlistenFocusWakeword?.();
  unlistenFocusWakeword = null;
});
</script>

<template>
  <div class="app-window">
    <!-- Tabs Navigation -->
    <div v-if="!showWizard" class="nav-container">
      <nav class="pill-tabs">
        <button :class="{ active: activeTab === 'general' }" @click="activeTab = 'general'">
          <span class="icon">⚙️</span>
          {{ t('config.tab_general') }}
        </button>
        <button :class="{ active: activeTab === 'sensory' }" @click="activeTab = 'sensory'">
          <span class="icon">🎤</span>
          {{ t('config.tab_sensory') }}
        </button>
      </nav>
    </div>

    <div class="scroll-area">
      <!-- ==================== First-run Wizard ==================== -->
      <div v-if="showWizard" class="wizard-container">
        <div class="wizard-header">
          <div class="brand-logo">Agent3</div>
          <p class="wizard-subtitle">
            {{ wizardStage === 'wakeword' ? t('config.wizard_wakeword_subtitle') : t('config.wizard_subtitle') }}
          </p>
        </div>

      <div class="wizard-setup" v-if="wizardStage !== 'wakeword' && !wizardProviderType">
        <div class="provider-cards">
          <!-- OpenAI Card -->
          <div class="wizard-card" @click="wizardProviderType = 'openai'">
            <div class="card-icon">🧠</div>
            <h3>OpenAI</h3>
            <p>{{ t('config.wizard_openai_desc') }}</p>
          </div>
          <!-- Gemini Card -->
          <div class="wizard-card" @click="wizardProviderType = 'gemini'">
            <div class="card-icon">✨</div>
            <h3>Gemini (Google)</h3>
            <p>{{ t('config.wizard_gemini_desc') }}</p>
          </div>
        </div>
      </div>

      <div class="wizard-setup form-mode" v-else-if="wizardStage !== 'wakeword'">
        <button class="btn ghost wizard-back-btn" @click="wizardProviderType = null">
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M15 18l-6-6 6-6"/></svg>
          {{ t('config.wizard_back') }}
        </button>
        <div class="card wizard-form">
          <h2>{{ t('config.wizard_configure') }} {{ wizardProviderType === 'openai' ? 'OpenAI' : 'Gemini' }}</h2>
          <ol class="steps">
            <li>{{ t('config.wizard_step1_prefix') }} <a href="#" @click.prevent="openApiKeyPage">{{ t('config.wizard_step1_link') }}</a></li>
            <li>{{ t('config.wizard_step2') }}</li>
            <li>{{ t('config.wizard_step3') }}</li>
          </ol>
          <div class="input-group">
            <input 
              v-model="wizardApiKey" 
              type="password" 
              placeholder="sk-..." 
              @keyup.enter="quickConnect"
            />
          </div>
          <button class="btn primary full-width mt-12" :disabled="wizardLoading" @click="quickConnect">
            {{ wizardLoading ? t('config.wizard_connecting') : t('config.wizard_connect_btn') }}
          </button>
          <!-- Status Tip -->
          <p v-if="statusMsg" class="status-tip">{{ statusMsg }}</p>
        </div>
      </div>

      <div class="wizard-setup form-mode" v-else>
        <div class="card wizard-form">
          <h2>{{ t('config.wizard_wakeword_h2') }}</h2>
          <p class="desc">{{ t('config.wizard_wakeword_desc') }}</p>

          <div v-if="!wizardWakewordExpanded" class="wizard-choice-group">
            <button class="btn primary full-width" @click="wizardWakewordExpanded = true">
              {{ t('config.wizard_wakeword_setup_now') }}
            </button>
            <button class="btn ghost full-width" @click="finishWizard(true)">
              {{ t('config.wizard_wakeword_skip') }}
            </button>
          </div>

          <div v-else class="wizard-wakeword-setup">
            <div class="sub-section wizard-sub-section">
              <h3>{{ t('config.wakeword_record_h3') }}</h3>
              <p class="desc">{{ t('config.wizard_wakeword_record_hint') }}</p>
              <div class="progress-wrap" aria-hidden="true">
                <div class="progress-bar" :style="{ width: `${wakewordProgress}%` }"></div>
              </div>
              <div class="action-row">
                <button v-if="!wakewordRecording" class="btn outline" @click="startRecording">
                  <span class="dot red"></span> {{ t('config.wakeword_record_start') }}
                </button>
                <button v-else class="btn recording pulse" @click="stopRecording">
                  {{ t('config.wakeword_record_stop') }}
                </button>
                <span class="badge count">{{ wakewordSampleCount }}/3</span>
              </div>
              <p v-if="wakewordLastDuration !== null" class="hint-line">
                {{ t('config.wakeword_last_sample_prefix') }} {{ wakewordLastDuration.toFixed(1) }}s · {{ wakewordLastQualityHint }}
              </p>
            </div>

            <div v-if="wakewordSampleCount >= 3" class="sub-section wizard-sub-section fade-in">
              <h3>{{ t('config.wakeword_train_h3') }}</h3>
              <div class="input-group">
                <label>{{ t('config.wizard_wakeword_name_label') }}</label>
                <div class="input-group-inline">
                  <input v-model="wakewordTrainName" :placeholder="t('config.wizard_wakeword_name_placeholder')" @keyup.enter="trainModel" />
                  <button class="btn primary" :disabled="wakewordTraining" @click="trainModel">
                    {{ wakewordTraining ? t('config.wakeword_training') : t('config.wizard_wakeword_train_btn') }}
                  </button>
                </div>
              </div>
            </div>

            <div class="wizard-choice-group mt-12">
              <button class="btn primary full-width" :disabled="!wakewordActiveModel" @click="finishWizard(false)">
                {{ t('config.wizard_wakeword_finish') }}
              </button>
              <button class="btn ghost full-width" @click="finishWizard(true)">
                {{ t('config.wizard_wakeword_skip') }}
              </button>
            </div>
          </div>
        </div>
      </div>
    </div>

      <!-- ==================== Advanced Config (shown when config exists) ==================== -->
      <div v-else class="settings-content">
        <!-- ==================== General ==================== -->
        <div v-show="activeTab === 'general'" class="fade-in">
          <section ref="wakewordSectionEl" class="card">
            <h2>{{ t('config.general_name_h2') }}</h2>
            <p class="desc">{{ t('config.general_name_hint') }}</p>
            <div class="input-group-inline">
              <input v-model="agentName" :placeholder="t('config.general_name_placeholder')" @keyup.enter="saveAgentName" />
              <button class="btn primary" @click="saveAgentName">{{ t('config.general_name_save') }}</button>
            </div>
          </section>

          <!-- Language Selector -->
          <section class="card">
            <h2>{{ t('config.general_lang_h2') }}</h2>
            <p class="desc">{{ t('config.general_lang_hint') }}</p>
            <div class="input-group">
              <select :value="locale" @change="changeLanguage(($event.target as HTMLSelectElement).value as Locale)">
                <option value="en">English (EN)</option>
                <option value="zh">简体中文 (ZH)</option>
              </select>
            </div>
          </section>

          <!-- Autostart -->
          <section class="card">
            <div class="card-header">
              <div>
                <h2>{{ t('config.autostart_h2') }}</h2>
                <p class="desc">{{ t('config.autostart_hint') }}</p>
              </div>
              <label class="switch">
                <input type="checkbox" v-model="autostartEnabled" @change="toggleAutostart" />
                <span class="slider"></span>
              </label>
            </div>
          </section>

          <!-- Wakeword Settings -->
          <section class="card">
            <div class="card-header">
              <div>
                <h2>{{ t('config.wakeword_h2') }}</h2>
                <p class="desc">{{ t('config.wakeword_hint') }}</p>
              </div>
              <label class="switch">
                <input type="checkbox" v-model="wakewordEnabled" @change="toggleWakeword" />
                <span class="slider"></span>
              </label>
            </div>

            <div v-if="wakewordSetupSkipped && !wakewordEnabled" class="alert info">
              {{ t('config.wakeword_resume_hint') }}
            </div>

            <!-- Record Samples -->
            <div class="sub-section">
              <h3>{{ t('config.wakeword_record_h3') }}</h3>
              <p class="desc">{{ t('config.wakeword_record_hint') }}</p>
              <div class="progress-wrap" aria-hidden="true">
                <div class="progress-bar" :style="{ width: `${wakewordProgress}%` }"></div>
              </div>
              <div class="action-row">
                <button v-if="!wakewordRecording" class="btn outline" @click="startRecording">
                  <span class="dot red"></span> {{ t('config.wakeword_record_start') }}
                </button>
                <button v-else class="btn recording pulse" @click="stopRecording">
                  {{ t('config.wakeword_record_stop') }}
                </button>
                <span class="badge count">{{ t('config.wakeword_record_count_prefix') }} {{ wakewordSampleCount }} {{ t('config.wakeword_record_count_suffix') }}</span>
              </div>
              <p v-if="wakewordLastDuration !== null" class="hint-line">
                {{ t('config.wakeword_last_sample_prefix') }} {{ wakewordLastDuration.toFixed(1) }}s · {{ wakewordLastQualityHint }}
              </p>
            </div>

            <!-- Train Model -->
            <div v-if="wakewordSampleCount >= 3" class="sub-section fade-in">
              <h3>{{ t('config.wakeword_train_h3') }}</h3>
              <div class="input-group">
                <label>{{ t('config.wakeword_train_name_label') }}</label>
                <div class="input-group-inline">
                  <input v-model="wakewordTrainName" :placeholder="t('config.wakeword_train_name_placeholder')" @keyup.enter="trainModel" />
                  <button class="btn primary" :disabled="wakewordTraining" @click="trainModel">
                    {{ wakewordTraining ? t('config.wakeword_training') : t('config.wakeword_train_btn') }}
                  </button>
                </div>
              </div>
            </div>

            <!-- Existing Model List -->
            <div v-if="wakewordModels.length > 0" class="sub-section">
              <h3>{{ t('config.wakeword_models_h3') }}</h3>
              <div class="list-group">
                <div v-for="m in wakewordModels" :key="m.path" class="list-item" :class="{ active: wakewordActiveModel === m.path }">
                  <div class="item-main">
                    <span class="item-title">{{ m.name }}</span>
                    <span class="tag file">.rpw</span>
                  </div>
                  <div class="item-actions">
                    <button v-if="wakewordActiveModel !== m.path" @click="activateModel(m)" class="btn ghost sm">{{ t('config.wakeword_activate') }}</button>
                    <span v-else class="tag success">{{ t('config.wakeword_activated') }}</span>
                    <button @click="deleteModel(m)" class="btn ghost danger sm">{{ t('config.wakeword_delete') }}</button>
                  </div>
                </div>
              </div>
            </div>
          </section>
        </div>

        <!-- ==================== Voice Service ==================== -->
        <div v-show="activeTab === 'sensory'" class="fade-in">
          <div v-if="realtimeProviders.length === 0" class="alert info">
            {{ t('config.sensory_banner') }}
          </div>

          <section class="card" v-if="realtimeProviders.length > 0">
            <h2>{{ t('config.sensory_list_h2') }}</h2>
            <div class="list-group">
              <div v-for="p in realtimeProviders" :key="p.id" class="list-item" :class="{ active: p.is_active }">
                <div class="item-main">
                  <span class="item-title">{{ p.name }}</span>
                  <div class="item-tags">
                    <span class="tag brand">{{ p.provider_type === 'gemini' ? 'Gemini' : 'OpenAI' }}</span>
                    <span class="tag">{{ p.model }}</span>
                  </div>
                </div>
                <div class="item-actions">
                  <button v-if="!p.is_active" @click="activateProvider(p.id)" class="btn ghost sm">{{ t('config.wakeword_activate') }}</button>
                  <span v-else class="tag success">{{ t('config.wakeword_activated') }}</span>
                  <button @click="removeProvider(p.id)" class="btn ghost danger sm">✕</button>
                </div>
              </div>
            </div>
          </section>

          <button v-if="realtimeProviders.length > 0 && !showSensoryForm" class="btn dashed full-width" @click="showSensoryForm = true">
            ＋ {{ t('config.sensory_add_btn') }}
          </button>

          <section class="card glass-form" v-show="showSensoryForm || realtimeProviders.length === 0">
            <h2>{{ t('config.sensory_add_h2') }}</h2>
            <div class="form-grid">
              <div class="input-group">
                <label>{{ t('config.sensory_preset') }}</label>
                <div class="select-wrapper">
                  <select v-model="sensoryForm.preset" @change="applySensoryPreset">
                    <option value="openai">OpenAI Realtime</option>
                    <option value="gemini">Gemini Multimodal Live</option>
                  </select>
                </div>
              </div>
              <div class="input-group">
                <label>{{ t('config.sensory_name') }}</label>
                <input v-model="sensoryForm.name" />
              </div>
              <div class="input-group full">
                <label>{{ t('config.sensory_ws_url') }}</label>
                <input v-model="sensoryForm.base_url" :placeholder="t('config.sensory_ws_placeholder')" />
              </div>
              <div class="input-group">
                <label>{{ t('config.sensory_apikey') }}</label>
                <input v-model="sensoryForm.api_key" type="password" :placeholder="t('config.sensory_apikey_placeholder')" />
              </div>
              <div class="input-group">
                <label>{{ t('config.sensory_model') }}</label>
                <input v-model="sensoryForm.model" />
              </div>
            </div>
            <div class="form-actions right">
              <button v-if="realtimeProviders.length > 0" class="btn ghost" @click="showSensoryForm = false">{{ t('config.add_cancel') }}</button>
              <button class="btn primary" @click="addProvider()">{{ t('config.sensory_add_btn') }}</button>
            </div>
          </section>

          <section class="card">
            <h2>{{ t('config.voice_h2') }}</h2>
            <p class="desc">{{ t('config.voice_hint') }}</p>
            <div class="grid-picker">
              <button
                v-for="v in activeVoiceList"
                :key="v.name"
                class="grid-item"
                :class="{ active: selectedVoice === v.name || (!selectedVoice && v.name === 'Aoede') }"
                :disabled="voiceSwitching"
                @click="switchVoice(v.name)"
              >
                <div class="avatar">
                  <svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5"><path d="M12 2a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3Z"/><path d="M19 10v2a7 7 0 0 1-14 0v-2M12 19v4M8 23h8"/></svg>
                </div>
                <span class="name">{{ v.name }}</span>
                <span class="sub">{{ t(v.descKey) }}</span>
              </button>
            </div>
          </section>
        </div>
      </div>

      <!-- Toast Notification -->
      <transition name="toast">
        <div v-if="statusMsg" class="toast-popup">
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M20 6L9 17l-5-5"/></svg>
          {{ statusMsg }}
        </div>
      </transition>
    </div> <!-- scroll-area ends -->
  </div> <!-- app-window ends -->
</template>

<style>
:root {
  --c-bg: #101014;
  --c-glass: rgba(255, 255, 255, 0.05);
  --c-glass-hover: rgba(255, 255, 255, 0.08);
  --c-border: rgba(255, 255, 255, 0.1);
  --c-text-main: #f0f0f4;
  --c-text-muted: #9494a0;
  --c-brand-1: #FF512F;
  --c-brand-2: #DD2476;
  --c-danger: #ea4e60;
  --c-success: #28c76f;
  --radius-lg: 16px;
  --radius-md: 10px;
  --radius-sm: 6px;
  --shadow-float: 0 10px 40px rgba(0,0,0,0.5);
  --font-sans: "Inter", -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
}

* { box-sizing: border-box; margin: 0; padding: 0; }

html, body {
  width: 100%; height: 100%;
  background: radial-gradient(circle at top left, #2a1124 0%, #101014 40%, #0d0a10 100%) !important;
  overflow: hidden;
  font-family: var(--font-sans);
  color: var(--c-text-main);
}

body {
  display: flex; align-items: stretch; justify-content: stretch;
}

#config-app {
  width: 100%; height: 100%; display: flex; flex-direction: column;
}

/* App Window Container */
.app-window {
  flex: 1;
  display: flex;
  flex-direction: column;
  overflow: hidden;
  padding-top: 16px;
}

/* Tabs */
.nav-container { padding: 0 24px; margin-bottom: 8px; }
.pill-tabs {
  display: inline-flex; background: var(--c-glass);
  padding: 4px; border-radius: var(--radius-md); gap: 4px;
}
.pill-tabs button {
  background: transparent; border: none; padding: 6px 16px;
  color: var(--c-text-muted); font-size: 13px; font-weight: 500;
  border-radius: var(--radius-sm); cursor: pointer; transition: all 0.2s;
  display: flex; align-items: center; gap: 6px;
}
.pill-tabs button:hover { color: var(--c-text-main); }
.pill-tabs button.active {
  background: rgba(221, 36, 118, 0.15);
  color: var(--c-text-main);
  box-shadow: 0 2px 8px rgba(0,0,0,0.2);
}

/* Scroll Area */
.scroll-area {
  flex: 1; overflow-y: auto; overflow-x: hidden;
  padding: 16px 24px 32px; position: relative;
}
.scroll-area::-webkit-scrollbar { width: 6px; }
.scroll-area::-webkit-scrollbar-track { background: transparent; }
.scroll-area::-webkit-scrollbar-thumb { background: var(--c-glass); border-radius: 4px; }

/* Settings Content Layout */
.settings-content { display: flex; flex-direction: column; gap: 24px; width: 100%; box-sizing: border-box; }
.card {
  background: var(--c-glass); border: 1px solid var(--c-border);
  border-radius: var(--radius-md); padding: 24px;
}
.card-header { display: flex; align-items: flex-start; justify-content: space-between; margin-bottom: 20px; }
h2 { font-size: 16px; font-weight: 600; margin-bottom: 6px; }
h3 { font-size: 14px; font-weight: 500; margin-bottom: 8px; color: var(--c-text-main); }
.desc { font-size: 13px; color: var(--c-text-muted); line-height: 1.6; margin-bottom: 20px; }

/* Inputs & Forms */
.input-group { display: flex; flex-direction: column; gap: 8px; width: 100%; box-sizing: border-box; margin-bottom: 20px; }
.input-group:last-child { margin-bottom: 0; }
.input-group label { font-size: 13px; font-weight: 500; color: var(--c-text-muted); }
.input-group-inline { display: flex; gap: 12px; align-items: stretch; width: 100%; box-sizing: border-box; margin-bottom: 20px; }
.input-group-inline:last-child { margin-bottom: 0; }
input, select {
  background: rgba(0,0,0,0.2); border: 1px solid var(--c-border);
  color: var(--c-text-main); font-size: 14px; padding: 10px 14px;
  border-radius: var(--radius-sm); outline: none; transition: 0.2s;
  width: 100%;
}
input:focus, select:focus {
  border-color: var(--c-brand-2); background: rgba(0,0,0,0.4);
}
.select-wrapper { position: relative; }

/* Buttons */
.btn {
  display: inline-flex; align-items: center; justify-content: center;
  padding: 10px 20px; font-size: 13px; font-weight: 500;
  border-radius: var(--radius-sm); cursor: pointer; transition: all 0.2s; border: none;
  white-space: nowrap; flex-shrink: 0;
}
.btn:disabled { opacity: 0.5; cursor: not-allowed; pointer-events: none; }
.btn.primary { background: linear-gradient(135deg, var(--c-brand-1), var(--c-brand-2)); color: white; }
.btn.primary:not(:disabled):hover { opacity: 0.9; box-shadow: 0 4px 12px rgba(221, 36, 118, 0.4); transform: translateY(-1px); }
.btn.primary:not(:disabled):active { transform: translateY(1px); }
.btn.ghost { background: var(--c-glass); color: var(--c-text-main); }
.btn.ghost:not(:disabled):hover { background: var(--c-glass-hover); }
.btn.outline { background: transparent; border: 1px solid var(--c-border); color: var(--c-text-main); }
.btn.outline:not(:disabled):hover { border-color: var(--c-text-main); }
.btn.dashed { background: transparent; border: 1px dashed var(--c-border); color: var(--c-text-muted); }
.btn.dashed:not(:disabled):hover { border-color: var(--c-text-main); color: var(--c-text-main); }
.btn.sm { padding: 6px 12px; font-size: 12px; }
.btn.danger { color: var(--c-danger); }
.full-width { width: 100%; }
.right { justify-content: flex-end; }
.action-row { display: flex; align-items: center; gap: 16px; margin-top: 12px; }

/* Lists & Items */
.list-group { display: flex; flex-direction: column; gap: 8px; margin-top: 16px; }
.list-item {
  display: flex; align-items: center; justify-content: space-between;
  padding: 12px 16px; background: rgba(0,0,0,0.2); border-radius: var(--radius-sm);
  border: 1px solid transparent; transition: 0.2s;
}
.list-item:hover { background: rgba(0,0,0,0.3); }
.list-item.active { border-color: var(--c-brand-2); background: rgba(221,36,118,0.05); }
.item-main { display: flex; align-items: center; gap: 12px; }
.item-title { font-weight: 500; font-size: 14px; }
.item-actions { display: flex; gap: 8px; align-items: center; }

/* Tags */
.tag { font-size: 11px; padding: 2px 8px; border-radius: 12px; background: var(--c-glass); }
.tag.brand { background: rgba(255, 81, 47, 0.15); color: var(--c-brand-1); }
.tag.success { background: rgba(40, 199, 111, 0.15); color: var(--c-success); }
.tag.file { font-family: monospace; color: var(--c-text-muted); }

/* Switch / Toggle */
.switch { position: relative; display: inline-block; width: 44px; height: 24px; }
.switch input { opacity: 0; width: 0; height: 0; }
.slider { position: absolute; cursor: pointer; top: 0; left: 0; right: 0; bottom: 0; background: var(--c-glass); transition: .3s; border-radius: 24px; border: 1px solid var(--c-border); }
.slider:before { position: absolute; content: ""; height: 16px; width: 16px; left: 3px; bottom: 3px; background: var(--c-text-muted); transition: .3s; border-radius: 50%; }
input:checked + .slider { background: var(--c-brand-2); border-color: transparent; }
input:checked + .slider:before { transform: translateX(20px); background: #fff; }

/* Animations & Extras */
.fade-in { animation: fadeIn 0.3s ease; width: 100%; display: flex; flex-direction: column; gap: 16px; }
@keyframes fadeIn { from { opacity: 0; transform: translateY(5px); } to { opacity: 1;} }
.sub-section { padding-top: 24px; margin-top: 24px; border-top: 1px solid rgba(255,255,255,0.05); }
.alert { padding: 12px 16px; border-radius: var(--radius-sm); font-size: 13px; margin-bottom: 20px; }
.alert.info { background: rgba(255,255,255,0.05); }
.alert.warn { background: rgba(255, 81, 47, 0.1); color: #ffb8a8; }
.dot.red { display: inline-block; width: 8px; height: 8px; border-radius: 50%; background: var(--c-danger); margin-right: 6px; }
.pulse { animation: pulseWarning 1.5s infinite; background: rgba(234,78,96,0.2) !important; color: var(--c-danger) !important; border: 1px solid var(--c-danger); }
@keyframes pulseWarning { 0%, 100% { opacity: 1; } 50% { opacity: 0.6; } }
.badge.count { font-family: monospace; color: var(--c-text-muted); font-size: 12px; }

.progress-wrap {
  width: 100%;
  height: 6px;
  border-radius: 999px;
  background: rgba(255, 255, 255, 0.08);
  overflow: hidden;
  margin: 8px 0 10px;
}

.progress-bar {
  height: 100%;
  border-radius: 999px;
  background: linear-gradient(90deg, var(--c-brand-1), var(--c-brand-2));
  transition: width 0.25s ease;
}

.hint-line {
  margin-top: 8px;
  font-size: 12px;
  color: var(--c-text-muted);
}

/* Forms Grid */
.glass-form { margin-top: 16px; width: 100%; box-sizing: border-box; }
.form-grid { display: flex; flex-direction: column; gap: 16px; margin-top: 12px; width: 100%; }
.form-grid .input-group { margin-bottom: 0; }
.form-actions { display: flex; gap: 12px; margin-top: 24px; }

/* Voices grid */
.grid-picker { display: grid; grid-template-columns: repeat(auto-fill, minmax(130px, 1fr)); gap: 12px; margin-top: 16px; }
.grid-item {
  display: flex; flex-direction: column; align-items: center; padding: 16px 12px;
  background: rgba(0,0,0,0.2); border: 1px solid transparent; border-radius: var(--radius-sm);
  cursor: pointer; transition: 0.2s; gap: 8px;
}
.grid-item:hover { background: rgba(0,0,0,0.3); border-color: var(--c-border); }
.grid-item.active { border-color: var(--c-brand-2); background: rgba(221,36,118,0.1); }
.avatar { color: var(--c-text-muted); }
.grid-item.active .avatar { color: var(--c-brand-2); }
.grid-item .name { font-weight: 500; font-size: 13px; color: var(--c-text-main); }
.grid-item .sub { font-size: 11px; color: var(--c-text-muted); text-align: center; }

/* Toast */
.toast-popup {
  position: absolute; bottom: 20px; left: 50%; transform: translateX(-50%);
  background: var(--c-text-main); color: #111; padding: 10px 20px;
  border-radius: 20px; font-size: 13px; font-weight: 500;
  display: flex; align-items: center; gap: 8px;
  box-shadow: 0 4px 15px rgba(0,0,0,0.3); z-index: 100;
}
.toast-enter-active, .toast-leave-active { transition: all 0.3s ease; }
.toast-enter-from, .toast-leave-to { opacity: 0; transform: translate(-50%, 20px); }

/* Wizard */
.wizard-container { display: flex; flex-direction: column; align-items: center; padding-top: 40px; text-align: center; }
.brand-logo { font-size: 32px; font-weight: 800; background: linear-gradient(135deg, var(--c-brand-1), var(--c-brand-2)); -webkit-background-clip: text; -webkit-text-fill-color: transparent; margin-bottom: 12px; }
.wizard-subtitle { color: var(--c-text-muted); margin-bottom: 40px; }

.provider-cards {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
  gap: 20px;
  width: 100%;
  max-width: 500px;
}

.wizard-card {
  display: flex;
  flex-direction: column;
  align-items: center;
  padding: 24px;
  background: rgba(255, 255, 255, 0.03);
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: var(--radius-md, 12px);
  cursor: pointer;
  transition: all 0.2s ease;
  text-align: center;
}

.wizard-card:hover {
  background: rgba(255, 255, 255, 0.08);
  border-color: var(--c-brand-2);
  transform: translateY(-2px);
}

.wizard-card .card-icon {
  font-size: 32px;
  margin-bottom: 12px;
}

.wizard-card h3 {
  margin: 0 0 8px 0;
  font-size: 18px;
  color: var(--c-text-main);
}

.wizard-card p {
  margin: 0;
  font-size: 13px;
  color: var(--c-text-muted);
}

.wizard-setup {
  display: flex;
  flex-direction: column;
  align-items: center;
  width: 100%;
}

.wizard-setup.form-mode {
  max-width: 400px;
  align-items: flex-start;
}

.wizard-back-btn {
  margin-bottom: 20px;
  display: flex;
  align-items: center;
  gap: 8px;
  padding-left: 0;
  color: var(--c-text-muted);
}

.wizard-back-btn:hover {
  color: var(--c-text-main);
  background: transparent;
}

.wizard-form {
  width: 100%;
  box-sizing: border-box;
  text-align: left;
}

.wizard-form h2 {
  font-size: 20px;
  margin-bottom: 24px;
}

.steps {
  color: var(--c-text-muted);
  font-size: 14px;
  line-height: 1.8;
  margin: 0 0 24px 0;
  padding-left: 20px;
}

.steps li {
  margin-bottom: 8px;
}

.steps a {
  color: var(--c-brand-2);
  text-decoration: none;
  font-weight: 500;
}

.steps a:hover {
  text-decoration: underline;
}

.mt-12 {
  margin-top: 12px;
}

.status-tip {
  margin-top: 16px;
  color: var(--c-danger);
  font-size: 13px;
  text-align: center;
}

.wizard-choice-group {
  display: flex;
  flex-direction: column;
  gap: 10px;
  width: 100%;
}

.wizard-wakeword-setup {
  width: 100%;
}

.wizard-sub-section {
  margin-top: 12px;
  padding-top: 16px;
}
</style>
