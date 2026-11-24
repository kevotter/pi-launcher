<script setup>
import { computed, nextTick, onBeforeUnmount, onMounted, ref, shallowRef } from 'vue';
import Button from 'primevue/button';
import Divider from 'primevue/divider';
import ToggleSwitch from 'primevue/toggleswitch';
import TrayIcon from './TrayIcon.vue';
import iconUrl from '../icon.svg';
import { createToggleGate, createTrayBridge, formatUptime, measureTray } from './bridge.js';

// `starting` defaults to true: before the first native push the honest state is
// "starting", not "stopped" (which would also enable actions that need a port).
const state = shallowRef({ ready: false, starting: true, autostart: {} });
const pending = ref(false);
const localError = ref('');
const now = ref(Date.now());
const wrapper = ref(null);
const card = ref(null);
const content = ref(null);
let clock;
const startup = computed(() => state.value.autostart || {});
const known = computed(() => typeof startup.value.enabled === 'boolean' && !startup.value.conflict);
const enabled = computed(() => known.value && startup.value.enabled === true);
const disabled = computed(() => !known.value || pending.value);
const error = computed(() => localError.value || startup.value.error ||
  (!known.value ? '无法确认开机自启状态，请重新打开菜单。' : ''));
const startupTitle = computed(() => error.value || (pending.value ? '正在确认开机自启状态…' :
  (enabled.value ? '关闭开机自启' : '开启开机自启')));
// Four states, in the order the user experiences them. "无响应" only when the
// process is known to be alive: a dead process is reported by the watcher.
const phase = computed(() => {
  if (state.value.starting) return 'starting';
  if (state.value.ready) return state.value.healthy === false ? 'unresponsive' : 'running';
  return 'stopped';
});
const badge = computed(() => ({
  starting: { text: '启动中', hint: '正在启动本地服务' },
  running: { text: '运行中', hint: '本地服务已就绪' },
  unresponsive: { text: '无响应', hint: '本地服务进程仍在，但没有响应' },
  stopped: { text: '已停止', hint: '本地服务未在运行' },
}[phase.value]));
// Uptime lives in the badge tooltip: the header carries no second line, and
// "运行中" plus "本地服务已就绪" would only repeat each other.
const uptime = computed(() => {
  if (phase.value !== 'running' && phase.value !== 'unresponsive') return '';
  const value = formatUptime(state.value.startedAt, now.value);
  return value ? `已运行 ${value}` : '';
});
const badgeTitle = computed(() => [badge.value.hint, uptime.value].filter(Boolean).join(' · '));
const restartDisabled = computed(() => state.value.starting === true || state.value.canRestart === false);
const maxHeightStyle = computed(() => {
  const height = state.value.maxHeight;
  return typeof height === 'number' && Number.isFinite(height) && height > 24
    ? { '--tray-card-max-height': `${height - 24}px` } : {};
});
const groups = [
  [{ action: 'show', label: '显示主窗口' },
    { action: 'browser', label: '在浏览器中打开', needsReady: true },
    { action: 'copy', label: '复制 Web 地址', needsReady: true },
    { action: 'restart', label: '重启本地服务', restart: true }],
  [{ action: 'config-dir', label: '打开配置目录' },
    { action: 'log-dir', label: '打开日志目录' }],
];
const bridge = createTrayBridge({
  onState(value) {
    state.value = { ...state.value, ...value };
    // Resize-only payloads must not release an outstanding startup request.
    if (Object.hasOwn(value, 'autostart')) {
      pending.value = false;
      localError.value = '';
    }
  },
  getSize: () => measureTray(wrapper.value, card.value, content.value),
  afterRender: nextTick,
});
const acceptClick = createToggleGate();
let toggleTimer;

function startupClick(event) {
  // Cancel the browser's checkbox default BEFORE PrimeVue can receive change.
  // PrimeVue change/update:modelValue events have no useful mouse click count.
  // We deliberately never write its model: only the verified native reply does.
  event.preventDefault();
  event.stopImmediatePropagation();
  if (disabled.value || !acceptClick(event)) return;
  pending.value = true;
  // A canceled checkbox click rolls its DOM value back AFTER event listeners.
  // Send in the next task so even an immediate native reply cannot be undone
  // by that browser default-action rollback (real Edge/WebView2 behavior).
  toggleTimer = setTimeout(() => {
    toggleTimer = undefined;
    if (!bridge.send('autostart-toggle')) {
      pending.value = false;
      localError.value = '无法发送自启设置请求，请重新打开菜单。';
    }
  }, 0);
}

function startupEnter(event) {
  if (event.key !== 'Enter') return;
  event.preventDefault();
  if (!event.repeat && !disabled.value) event.target.click();
}

onMounted(() => {
  // Uptime is derived from the wall-clock start the native side reports, so a
  // stale state push cannot freeze it. 20 s is enough for a minute-resolution
  // label and costs nothing while the popover is hidden.
  clock = setInterval(() => { now.value = Date.now(); }, 20000);
  void bridge.mounted([wrapper.value, card.value, content.value]);
});
onBeforeUnmount(() => {
  clearTimeout(toggleTimer);
  clearInterval(clock);
  bridge.dispose();
});
</script>

<template>
  <main ref="wrapper" class="tray-wrapper" :style="maxHeightStyle" aria-label="Pi Launcher 菜单">
    <section ref="card" class="tray-card">
      <div ref="content" class="tray-content">
        <header class="tray-head">
          <img :src="iconUrl" alt="" width="20" height="20">
          <div class="head-text">
            <h1>Pi Launcher</h1>
          </div>
          <span id="badge" class="status-tag" :data-status="phase" :title="badgeTitle"
            role="status" aria-live="polite">
            <span class="status-dot" aria-hidden="true" />
            <span id="badge-text">{{ badge.text }}</span>
          </span>
        </header>

        <template v-for="(group, index) in groups" :key="index">
          <Divider />
          <Button v-for="item in group" :id="`action-${item.action}`" :key="item.action"
            class="tray-action" :data-action="item.action" text
            :disabled="item.needsReady ? !state.ready : (item.restart ? restartDisabled : false)"
            @click="bridge.send(item.action)">
            <TrayIcon :name="item.action" /><span class="action-label">{{ item.label }}</span>
          </Button>
        </template>

        <Divider />
        <div class="startup-row" :class="{ 'startup-unknown': !known, 'startup-pending': pending }"
          :title="startupTitle" @click.capture="startupClick" @keydown="startupEnter">
          <TrayIcon name="autostart" />
          <label id="autostart-label" for="action-autostart" class="action-label">开机自启</label>
          <ToggleSwitch input-id="action-autostart" :model-value="enabled" :disabled="disabled"
            aria-labelledby="autostart-label"
            :pt="{ input: { role: known ? 'switch' : 'checkbox', 'aria-checked': known ? String(enabled) : 'mixed', 'aria-describedby': 'autostart-status', 'aria-busy': pending, title: startupTitle } }">
            <template #handle><span v-if="!known" class="unknown-mark" aria-hidden="true">?</span></template>
          </ToggleSwitch>
        </div>
        <span id="autostart-status" class="sr-only" role="status" aria-live="polite" aria-atomic="true">{{ error }}</span>
        <Button id="action-quit" class="tray-action tray-quit" data-action="quit" text @click="bridge.send('quit')">
          <TrayIcon name="quit" /><span class="action-label">退出</span>
        </Button>
      </div>
    </section>
  </main>
</template>
