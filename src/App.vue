<template>
	<div class="app">
		<!-- Header -->
		<div class="topbar">
			<div class="title">{{ activeTab === 'control' ? 'Панель управления' : 'Настройки' }}</div>
		</div>

		<!-- Content -->
		<div class="content">
			<!-- CONTROL TAB -->
			<div v-if="activeTab === 'control'" class="page">
				<div class="card">
					<div class="card-title">Профиль</div>

					<div v-if="loadingProfiles" class="muted">Загрузка…</div>

					<div v-else class="list">
						<label
							v-for="name in profiles"
							:key="name"
							class="row"
						>
							<input
								class="radio"
								type="radio"
								name="profile"
								:value="name"
								v-model="selectedProfile"
								@change="onSelectProfile(name)"
							/>
							<span class="row-text">{{ name }}</span>
						</label>

						<div v-if="!profiles.length" class="muted">
							Профили не загружены. Нажмите «Обновить конфиги» в настройках.
						</div>
					</div>
				</div>
			</div>

			<!-- SETTINGS TAB -->
			<div v-else class="page">
				<!-- Access Key -->
				<div class="card">
					<div class="card-title">Ключ доступа</div>

					<input
						class="input"
						type="text"
						placeholder="Введите ключ доступа…"
						v-model="accessKey"
						@blur="saveAccessKey"
						@keyup.enter="saveAccessKey"
					/>

					<div class="actions">
						<button class="btn" @click="refreshConfigs" :disabled="loadingConfigs">
							{{ loadingConfigs ? 'Обновление…' : 'Обновить конфиги' }}
						</button>
					</div>

					<div v-if="errorText" class="error">{{ errorText }}</div>
				</div>

				<!-- Logs -->
				<div class="card">
					<div class="row-between">
						<div>
							<div class="card-title">Логи</div>
							<div class="muted">Откроется файл логов приложения</div>
						</div>
						<button class="btn btn-ghost" @click="openLogs">Открыть</button>
					</div>
				</div>

				<div class="card">
					<div class="cardTitle">Приложение</div>

					<label class="row">
						<input
							type="checkbox"
							v-model="autostartEnabled"
							:disabled="autostartLoading"
							@change="saveAutostart"
						/>
						<span>Автозапуск при старте системы</span>
					</label>

					<div class="muted" style="margin-top:6px" v-if="autostartNote">
						{{ autostartNote }}
					</div>
				</div>

				<!-- apps settings -->
				<div class="card">
					<div class="cardTitle">Маршрутизация</div>

					<label class="row">
						<input type="checkbox" v-model="split.enabled" @change="saveSplit"/>
						<span>Раздельная маршрутизация</span>
					</label>

					<div class="grid2">
						<div>
							<div class="smallTitle">Outbound proxy</div>
							<input class="input" v-model="split.proxyOutbound" @change="saveSplit" placeholder="proxy"/>
						</div>
						<div>
							<div class="smallTitle">Outbound direct</div>
							<input class="input" v-model="split.directOutbound" @change="saveSplit"
								   placeholder="direct"/>
						</div>
					</div>

					<!--<div class="splitBlock">-->
					<!--	<div class="smallTitle">Не пускать через прокси (apps → direct)</div>-->
					<!--	<div class="row">-->
					<!--		&lt;!&ndash;<input class="input" v-model="newBypassApp"&ndash;&gt;-->
					<!--		&lt;!&ndash;	   placeholder="Windows: chrome.exe / macOS: Safari"/>&ndash;&gt;-->
					<!--		&lt;!&ndash;<button class="btn" @click="addTo('bypassApps','newBypassApp')">Добавить</button>&ndash;&gt;-->
					<!--		<button class="btn btn-ghost" @click="openAppsPicker('bypassApps')">Выбрать из запущенных-->
					<!--		</button>-->
					<!--	</div>-->
					<!--	<div class="chips">-->
					<!--	  <span class="chip" v-for="a in split.bypassApps" :key="a">-->
					<!--		{{ a }} <button class="chipX" @click="removeFrom('bypassApps', a)">×</button>-->
					<!--	  </span>-->
					<!--	</div>-->
					<!--</div>-->

					<div class="splitBlock">
						<div class="smallTitle">Пускать через прокси (apps → proxy)</div>
						<div class="row">
							<!--<input class="input" v-model="newProxyApp"-->
							<!--	   placeholder="Windows: telegram.exe / macOS: Telegram"/>-->
							<!--<button class="btn" @click="addTo('proxyApps','newProxyApp')">Добавить</button>-->
							<button class="btn btn-ghost" @click="openAppsPicker('proxyApps')">Выбрать из запущенных
							</button>
						</div>
						<div class="chips">
						  <span class="chip" v-for="a in split.proxyApps" :key="a">
							{{ a }} <button class="chipX" @click="removeFrom('proxyApps', a)">×</button>
						  </span>
						</div>
					</div>

					<!--<div class="splitBlock">-->
					<!--	<div class="smallTitle">Не пускать через прокси (domains → direct)</div>-->
					<!--	<div class="row">-->
					<!--		<input class="input" v-model="newBypassDomain" placeholder="например: apple.com"/>-->
					<!--		<button class="btn" @click="addTo('bypassDomains','newBypassDomain')">Добавить</button>-->
					<!--	</div>-->
					<!--	<div class="chips">-->
					<!--	  <span class="chip" v-for="d in split.bypassDomains" :key="d">-->
					<!--		{{ d }} <button class="chipX" @click="removeFrom('bypassDomains', d)">×</button>-->
					<!--	  </span>-->
					<!--	</div>-->
					<!--</div>-->

					<div class="splitBlock">
						<div class="smallTitle">Пускать через прокси (domains → proxy)</div>
						<div class="row">
							<input class="input" v-model="newProxyDomain" placeholder="например: google.com"/>
							<button class="btn" @click="addTo('proxyDomains','newProxyDomain')">Добавить</button>
						</div>
						<div class="chips">
						  <span class="chip" v-for="d in split.proxyDomains" :key="d">
							{{ d }} <button class="chipX" @click="removeFrom('proxyDomains', d)">×</button>
						  </span>
						</div>
					</div>

					<div class="splitBlock">
						<label class="row">
							<input
								type="checkbox"
								v-model="socks5Inbound"
								:disabled="!split.enabled"
								@change="saveSocks5Inbound"
							/>
							<span>Включить браузерный прокси</span>
						</label>
						<div class="muted" v-if="!split.enabled" style="margin-top:6px">
							Доступно только при включенной «Раздельной маршрутизации».
						</div>
					</div>
				</div>

				<!-- App section -->
				<!--<div class="card">-->
				<!--	<div class="card-title">Приложение</div>-->
				<!--	<div class="setting-row">-->
				<!--		<div class="setting-label">Отображать скорость в уведомлении</div>-->
				<!--		<select class="select" v-model="showSpeed">-->
				<!--			<option value="on">Включено</option>-->
				<!--			<option value="off">Выключено</option>-->
				<!--		</select>-->
				<!--	</div>-->
				<!--	<div class="setting-row">-->
				<!--		<div class="setting-label">Автоматическая проверка обновлений</div>-->
				<!--		<select class="select" v-model="autoUpdate">-->
				<!--			<option value="on">Включено</option>-->
				<!--			<option value="off">Выключено</option>-->
				<!--		</select>-->
				<!--	</div>-->
				<!--	<div class="actions grid3">-->
				<!--		<button class="btn btn-ghost" @click="checkUpdates">Проверить обновление</button>-->
				<!--		<button class="btn btn-ghost" @click="openPrivacy">Политика конфиденциальности</button>-->
				<!--		<button class="btn btn-ghost" @click="openAbout">О приложении</button>-->
				<!--	</div>-->
				<!--</div>-->
				<!-- Core section -->
				<!--<div class="card">-->
				<!--<div class="card-title">Ядро</div>-->
				<!--	<div class="kv">-->
				<!--		<div class="k">Версия</div>-->
				<!--		<div class="v">{{ coreVersion }}</div>-->
				<!--	</div>-->
				<!--	<div class="kv">-->
				<!--		<div class="k">Размер данных</div>-->
				<!--		<div class="v">{{ coreDataSize }}</div>-->
				<!--	</div>-->
				<!--	<div class="setting-row">-->
				<!--		<div class="setting-label">Ограничение памяти</div>-->
				<!--		<select class="select" v-model="memLimit">-->
				<!--			<option value="on">Включено</option>-->
				<!--			<option value="off">Выключено</option>-->
				<!--		</select>-->
				<!--	</div>-->
				<!--</div>-->
			</div>
		</div>

		<!-- Floating Start/Stop button (like play button) -->
		<button
			class="fab"
			:class="{ on: isRunning }"
			@click="toggleRun"
			:title="isRunning ? 'Остановить' : 'Запустить'"
		>
			<span v-if="!isRunning">▶</span>
			<span v-else>■</span>
		</button>

		<!-- Bottom nav -->
		<div class="bottom-nav">
			<button
				class="nav-btn"
				:class="{ active: activeTab === 'control' }"
				@click="activeTab = 'control'"
			>
				<div class="nav-ico">▦</div>
				<div class="nav-txt">Панель управления</div>
			</button>

			<button
				class="nav-btn"
				:class="{ active: activeTab === 'settings' }"
				@click="activeTab = 'settings'"
			>
				<div class="nav-ico">⚙</div>
				<div class="nav-txt">Настройки</div>
			</button>
		</div>
	</div>

	<!-- Apps Picker Modal -->
	<div v-if="appsModalOpen" class="modalOverlay" @click.self="closeAppsPicker">
		<div class="modal">
			<div class="modalHead">
				<div class="modalTitle">Запущенные приложения</div>
				<button class="btn btn-ghost" @click="closeAppsPicker">✕</button>
			</div>

			<div class="row">
				<input class="input" v-model="appsSearch" placeholder="Поиск: имя / путь / заголовок окна…"/>
				<button class="btn" @click="refreshRunningApps" :disabled="appsLoading">
					{{ appsLoading ? 'Обновление…' : 'Обновить' }}
				</button>
			</div>

			<div class="muted" v-if="appsLoading">Загрузка…</div>

			<div class="appsList" v-else>
				<button
					v-for="a in filteredRunningApps"
					:key="a.pid + ':' + (a.path || a.name)"
					class="appRow"
					@click="addRunningApp(a)"
				>
					<div class="appMain">
						<div class="appName">{{ a.name }}</div>
						<div class="appTitle" v-if="a.title">{{ a.title }}</div>
					</div>
					<div class="appPath">{{ a.path || '—' }}</div>
				</button>

				<div class="muted" v-if="!filteredRunningApps.length">
					Ничего не найдено
				</div>
			</div>

			<div class="modalHint muted">
				При выборе добавляется <b>путь</b> (process_path). Если путь недоступен — добавится имя процесса
				(process_name).
			</div>
		</div>
	</div>

</template>

<script lang="ts">
type SplitListKey = "bypassApps" | "proxyApps" | "bypassDomains" | "proxyDomains";
type InputKey = "newBypassApp" | "newProxyApp" | "newBypassDomain" | "newProxyDomain";

type SplitRoutingSettings = {
	enabled: boolean
	bypassApps: string[]
	proxyApps: string[]
	bypassDomains: string[]
	proxyDomains: string[]
	proxyOutbound: string
	directOutbound: string
}

type RunningApp = {
	pid: number
	name: string
	path?: string | null
	title?: string | null
}

function defaultSplit(): SplitRoutingSettings {
	return {
		enabled: false,
		bypassApps: [],
		proxyApps: [],
		bypassDomains: [],
		proxyDomains: [],
		proxyOutbound: "proxy",
		directOutbound: "direct",
	}
}

import {defineComponent} from 'vue'
import {invoke} from '@tauri-apps/api/core'
// import { open } from '@tauri-apps/plugin-shell' // если нет — убери openLogs() или используй opener plugin

export default defineComponent({
	name: 'App',
	data: () => ({
		activeTab: 'control' as 'control' | 'settings',

		isRunning: false,

		profiles: [] as string[],
		selectedProfile: '' as string,

		accessKey: '' as string,

		loadingProfiles: false,
		loadingConfigs: false,
		errorText: '' as string,

		// settings UI (пока просто UI, можно потом сохранять)
		showSpeed: 'on',
		autoUpdate: 'on',
		memLimit: 'on',

		coreVersion: '—',
		coreDataSize: '0 B',

		split: defaultSplit(),
		newBypassApp: "" as string,
		newProxyApp: "" as string,
		newBypassDomain: "" as string,
		newProxyDomain: "" as string,

		runningApps: [] as RunningApp[],
		appsLoading: false,
		appsModalOpen: false,
		appsModalTarget: null as null | SplitListKey, // куда добавляем: bypassApps или proxyApps
		appsSearch: "" as string,

		socks5Inbound: false,

		autostartEnabled: false,
		autostartOsEnabled: false,
		autostartLoading: false,
		autostartNote: '' as string,
	}),

	async created() {
		await this.bootstrap()
		await this.loadSplit()
		await this.loadSocks5Inbound()
	},

	methods: {
		async bootstrap() {
			try {
				this.errorText = ''
				this.loadingProfiles = true

				// состояние
				this.isRunning = await invoke<boolean>('get_state')

				// ключ
				this.accessKey = await invoke<string>('get_access_key')

				// профили из локального кеша (если ты сделал сохранение configs.json)
				const list = await invoke<string[]>('get_profiles')
				this.profiles = Array.isArray(list) ? list : []

				// выбранный профиль
				const selected = await invoke<string | null>('get_selected_profile')
				if (selected) this.selectedProfile = selected

			} catch (e: any) {
				this.errorText = String(e)
			} finally {
				this.loadingProfiles = false
			}
		},

		async toggleRun() {
			try {
				this.errorText = ''
				if (this.isRunning) {
					await invoke('singbox_stop_platform')
					this.isRunning = false
					return
				}
				if (!this.selectedProfile) {
					this.errorText = 'Выберите профиль'
					return
				}
				await invoke('singbox_start_platform')
				this.isRunning = true
			} catch (e: any) {
				this.errorText = String(e)
				this.isRunning = await invoke<boolean>('get_state').catch(() => false)
			}
		},

		async onSelectProfile(name: string) {
			try {
				this.errorText = ''
				this.selectedProfile = name
				await invoke('set_selected_profile', {profile: name}) // <-- без id
			} catch (e: any) {
				this.errorText = String(e)
			}
		},

		async saveAccessKey() {
			try {
				this.errorText = ''
				const key = (this.accessKey || '').trim()
				await invoke('set_access_key', {key})
			} catch (e: any) {
				this.errorText = String(e)
			}
		},

		async refreshConfigs() {
			try {
				this.errorText = ''
				this.loadingConfigs = true

				// Сохраним ключ перед загрузкой
				await this.saveAccessKey()

				const list = await invoke<string[]>('load_configs')
				this.profiles = Array.isArray(list) ? list : []

				// если раньше выбранный профиль отсутствует — сбросим
				if (this.selectedProfile && !this.profiles.includes(this.selectedProfile)) {
					this.selectedProfile = ''
					await invoke('set_selected_profile', {profile: ''}).catch(() => {
					})
				}

				// вернём на вкладку управления
				this.activeTab = 'control'
			} catch (e: any) {
				this.errorText = String(e)
			} finally {
				this.loadingConfigs = false
			}
		},

		async openLogs() {
			this.errorText = ''
			try {
				await invoke('open_logs')
			} catch (e: any) {
				this.errorText = String(e)
			}
		},
		async loadSplit(): Promise<void> {
			this.split = (await invoke<SplitRoutingSettings>("get_split_routing")) ?? defaultSplit();
		},

		async saveSplit(): Promise<void> {
			await invoke("set_split_routing", {split: this.split})
			// применить сразу
			if (this.isRunning) {
				await invoke("singbox_stop_platform")
				await invoke("singbox_start_platform")
				this.isRunning = true
			}
		},

		addTo(listName: SplitListKey, valueField: InputKey): void {
			const v = (this[valueField] || "").trim();
			if (!v) {
				return;
			}
			if (!this.split[listName].includes(v)) {
				this.split[listName].push(v);
			}
			this[valueField] = "";
			void this.saveSplit();
		},

		removeFrom(listName: SplitListKey, item: string): void {
			this.split[listName] = this.split[listName].filter((x) => x !== item);
			void this.saveSplit();
		},

		async saveSettings() {
			await invoke("set_access_key", {key: this.accessKey})
			await invoke("set_split_routing", {split: this.split})
			alert("Настройки сохранены")
		},

		checkUpdates() {
			// UI-заглушка
			this.errorText = 'Проверка обновлений: пока не реализовано'
		},

		openPrivacy() {
			// UI-заглушка
			this.errorText = 'Политика конфиденциальности: пока не реализовано'
		},

		openAbout() {
			// UI-заглушка
			this.errorText = 'О приложении: пока не реализовано'
		},

		async openAppsPicker(target: SplitListKey) {
			this.appsModalTarget = target
			this.appsModalOpen = true
			this.appsSearch = ""

			// грузим при открытии
			if (!this.runningApps.length) {
				await this.refreshRunningApps()
			}
		},

		async refreshRunningApps() {
			try {
				this.appsLoading = true
				this.runningApps = await invoke<RunningApp[]>("list_running_apps")
			} finally {
				this.appsLoading = false
			}
		},

		closeAppsPicker() {
			this.appsModalOpen = false
			this.appsModalTarget = null
		},

		addRunningApp(app: RunningApp) {
			if (!this.appsModalTarget) return

			// предпочтительно добавляем PATH (самое надёжное для sing-box: process_path)
			let value =
				(app.path && String(app.path).trim().length ? String(app.path).trim() : "") ||
				(app.name || "").trim()

			if (!value) return

			if (!this.split[this.appsModalTarget].includes(value)) {
				this.split[this.appsModalTarget].push(value)
				void this.saveSplit()
			}
		},

		async loadSocks5Inbound(): Promise<void> {
			this.socks5Inbound = await invoke<boolean>("get_socks5_inbound")
		},

		async saveSocks5Inbound(): Promise<void> {
			await invoke("set_socks5_inbound", {enabled: this.socks5Inbound})

			// применить сразу
			if (this.isRunning) {
				await invoke("singbox_stop_platform")
				await invoke("singbox_start_platform")
				this.isRunning = true
			}
		},

		async loadAutostart() {
			this.autostartNote = ''
			this.autostartLoading = true
			try {
				const st = await invoke<{ desired: boolean; enabled: boolean }>('get_autostart_status')
				this.autostartEnabled = !!st?.desired
				this.autostartOsEnabled = !!st?.enabled

				if (this.autostartOsEnabled !== this.autostartEnabled) {
					this.autostartNote = this.autostartEnabled
						? 'В системе автозапуск сейчас выключен. Включаю при следующем сохранении.'
						: 'В системе автозапуск сейчас включен. Отключаю при следующем сохранении.'
				} else {
					this.autostartNote = this.autostartEnabled ? 'В системе: включено.' : 'В системе: выключено.'
				}
			} catch (e: any) {
				this.autostartNote = String(e)
			} finally {
				this.autostartLoading = false
			}
		},

		async saveAutostart() {
			this.autostartNote = ''
			this.autostartLoading = true
			try {
				await invoke<void>('set_autostart_enabled', { enabled: this.autostartEnabled })
				await this.loadAutostart()
			} catch (e: any) {
				this.autostartNote = String(e)
			} finally {
				this.autostartLoading = false
			}
		},

	},

	computed: {
		filteredRunningApps(): RunningApp[] {
			const q = (this.appsSearch || "").trim().toLowerCase()
			if (!q) return this.runningApps

			return this.runningApps.filter(a => {
				const n = (a.name || "").toLowerCase()
				const p = (a.path || "").toLowerCase()
				const t = (a.title || "").toLowerCase()
				return n.includes(q) || p.includes(q) || t.includes(q)
			})
		},
	}
})
</script>

<style scoped>
:root {
	color-scheme: dark;
}

.app {
	height: 100vh;
	display: flex;
	flex-direction: column;
	background: #1e1e1f;
	color: #eaeaea;
	font-family: system-ui, -apple-system, Segoe UI, Roboto, Arial, sans-serif;
}

/* Top */
.topbar {
	padding: 18px 18px 8px;
}

.title {
	font-size: 26px;
	font-weight: 700;
	letter-spacing: 0.2px;
}

/* Content */
.content {
	width: 50vw;
	min-width: 300px;
	flex: 1;
	overflow: auto;
	padding: 10px 14px 90px;
}

.page {
	display: flex;
	flex-direction: column;
	gap: 14px;
}

.card {
	background: #2a2a2c;
	border-radius: 16px;
	padding: 14px;
	box-shadow: 0 2px 10px rgba(0, 0, 0, .25);
	border: 1px solid rgba(255, 255, 255, .06);
}

.card-title {
	font-weight: 700;
	font-size: 18px;
	margin-bottom: 10px;
}

.list {
	display: flex;
	flex-direction: column;
}

.row {
	display: flex;
	align-items: center;
	gap: 12px;
	padding: 12px 6px;
	border-top: 1px solid rgba(255, 255, 255, .08);
}

.row:first-of-type {
	border-top: none;
}

.row-text {
	font-size: 16px;
}

.radio {
	width: 18px;
	height: 18px;
	accent-color: #b48cff;
}

.input {
	width: 100%;
	border: none;
	outline: none;
	background: #242426;
	color: #f2f2f2;
	padding: 12px 12px;
	border-radius: 12px;
	border: 1px solid rgba(255, 255, 255, .08);
}

.actions {
	margin-top: 12px;
	display: flex;
	gap: 10px;
	flex-wrap: wrap;
}

.grid3 {
	display: grid;
	grid-template-columns: 1fr 1fr 1fr;
	gap: 10px;
}

@media (max-width: 520px) {
	.grid3 {
		grid-template-columns: 1fr;
	}
}

.btn {
	background: #3a3a3e;
	color: #fff;
	border: 1px solid rgba(255, 255, 255, .10);
	border-radius: 12px;
	padding: 10px 12px;
	cursor: pointer;
}

.btn:disabled {
	opacity: .6;
	cursor: default;
}

.btn-ghost {
	background: transparent;
}

.row-between {
	display: flex;
	justify-content: space-between;
	align-items: center;
	gap: 10px;
}

.setting-row {
	display: flex;
	align-items: center;
	justify-content: space-between;
	gap: 12px;
	padding: 10px 0;
	border-top: 1px solid rgba(255, 255, 255, .08);
}

.setting-row:first-of-type {
	border-top: none;
}

.setting-label {
	font-size: 14px;
	color: rgba(255, 255, 255, .85);
}

.select {
	background: #242426;
	color: #f2f2f2;
	border: 1px solid rgba(255, 255, 255, .08);
	border-radius: 10px;
	padding: 8px 10px;
	outline: none;
	min-width: 140px;
}

.kv {
	display: flex;
	justify-content: space-between;
	padding: 6px 0;
	border-top: 1px solid rgba(255, 255, 255, .08);
}

.kv:first-of-type {
	border-top: none;
}

.k {
	color: rgba(255, 255, 255, .7);
}

.v {
	color: rgba(255, 255, 255, .95);
}

.muted {
	color: rgba(255, 255, 255, .65);
	font-size: 13px;
	padding: 8px 0;
}

.error {
	margin-top: 10px;
	color: #ff8a8a;
	font-size: 13px;
}

/* Floating action button */
.fab {
	position: fixed;
	right: 18px;
	bottom: 78px;
	width: 56px;
	height: 56px;
	border-radius: 16px;
	border: none;
	background: #d9d9dd;
	color: #111;
	font-size: 22px;
	box-shadow: 0 10px 24px rgba(0, 0, 0, .35);
	cursor: pointer;
}

.fab.on {
	background: #b48cff;
	color: #111;
}

/* Bottom nav */
.bottom-nav {
	position: fixed;
	left: 0;
	right: 0;
	bottom: 0;
	height: 64px;
	display: flex;
	background: #2a2a2c;
	border-top: 1px solid rgba(255, 255, 255, .08);
}

.nav-btn {
	flex: 1;
	border: none;
	background: transparent;
	color: rgba(255, 255, 255, .65);
	display: flex;
	flex-direction: column;
	align-items: center;
	justify-content: center;
	gap: 3px;
	cursor: pointer;
}

.nav-btn.active {
	color: #fff;
}

.nav-ico {
	font-size: 18px;
	line-height: 18px;
}

.nav-txt {
	font-size: 12px;
}

.card {
	background: #1f1f1f;
	border-radius: 16px;
	padding: 16px;
	margin: 12px 0;
}

.cardTitle {
	font-size: 18px;
	font-weight: 700;
	margin-bottom: 12px;
}

.smallTitle {
	opacity: .8;
	font-size: 12px;
	margin: 10px 0 6px;
}

.row {
	display: flex;
	align-items: center;
	gap: 10px;
}

.grid2 {
	display: grid;
	grid-template-columns:1fr 1fr;
	gap: 12px;
}

.input {
	flex: 1;
	background: #2a2a2a;
	border: 1px solid #3a3a3a;
	color: #fff;
	border-radius: 10px;
	padding: 10px 12px;
}

.btn {
	background: #3b82f6;
	color: #fff;
	border: none;
	border-radius: 10px;
	padding: 10px 12px;
	cursor: pointer;
}

.splitBlock {
	margin-top: 12px;
}

.chips {
	display: flex;
	flex-wrap: wrap;
	gap: 8px;
	margin-top: 10px;
}

.chip {
	background: #2a2a2a;
	border: 1px solid #3a3a3a;
	border-radius: 999px;
	padding: 6px 10px;
	display: flex;
	align-items: center;
	gap: 8px;
}

.chipX {
	background: transparent;
	border: none;
	color: #fff;
	cursor: pointer;
	font-size: 16px;
	line-height: 1;
}

.modalOverlay {
	position: fixed;
	inset: 0;
	background: rgba(0, 0, 0, .45);
	display: flex;
	align-items: center;
	justify-content: center;
	padding: 16px;
	z-index: 9999;
}

.modal {
	width: min(900px, 100%);
	max-height: min(80vh, 760px);
	background: #14141a;
	border: 1px solid rgba(255, 255, 255, .08);
	border-radius: 14px;
	overflow: hidden;
	display: flex;
	flex-direction: column;
}

.modalHead {
	display: flex;
	align-items: center;
	justify-content: space-between;
	padding: 12px 12px;
	border-bottom: 1px solid rgba(255, 255, 255, .08);
}

.modalTitle {
	font-weight: 700;
}

.appsList {
	overflow: auto;
	padding: 10px;
	display: flex;
	flex-direction: column;
	gap: 8px;
}

.appRow {
	text-align: left;
	border: 1px solid rgba(255, 255, 255, .08);
	background: rgba(255, 255, 255, .03);
	border-radius: 12px;
	padding: 10px 12px;
	cursor: pointer;
	display: flex;
	flex-direction: column;
	gap: 6px;
}

.appRow:hover {
	background: rgba(255, 255, 255, .06);
}

.appMain {
	display: flex;
	align-items: baseline;
	justify-content: space-between;
	gap: 12px;
}

.appName {
	font-weight: 700;
}

.appTitle {
	color: rgba(255, 255, 255, .65);
	font-size: 12px;
}

.appPath {
	color: rgba(255, 255, 255, .55);
	font-size: 12px;
	word-break: break-all;
}

.modalHint {
	padding: 10px 12px;
	border-top: 1px solid rgba(255, 255, 255, .08);
}

</style>
