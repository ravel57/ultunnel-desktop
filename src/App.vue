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

				<!-- App section -->
				<div class="card">
					<div class="card-title">Приложение</div>

					<div class="setting-row">
						<div class="setting-label">Отображать скорость в уведомлении</div>
						<select class="select" v-model="showSpeed">
							<option value="on">Включено</option>
							<option value="off">Выключено</option>
						</select>
					</div>

					<div class="setting-row">
						<div class="setting-label">Автоматическая проверка обновлений</div>
						<select class="select" v-model="autoUpdate">
							<option value="on">Включено</option>
							<option value="off">Выключено</option>
						</select>
					</div>

					<div class="actions grid3">
						<button class="btn btn-ghost" @click="checkUpdates">Проверить обновление</button>
						<button class="btn btn-ghost" @click="openPrivacy">Политика конфиденциальности</button>
						<button class="btn btn-ghost" @click="openAbout">О приложении</button>
					</div>
				</div>

				<!-- Core section -->
				<div class="card">
					<div class="card-title">Ядро</div>

					<div class="kv">
						<div class="k">Версия</div>
						<div class="v">{{ coreVersion }}</div>
					</div>
					<div class="kv">
						<div class="k">Размер данных</div>
						<div class="v">{{ coreDataSize }}</div>
					</div>

					<div class="setting-row">
						<div class="setting-label">Ограничение памяти</div>
						<select class="select" v-model="memLimit">
							<option value="on">Включено</option>
							<option value="off">Выключено</option>
						</select>
					</div>
				</div>
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
</template>

<script lang="ts">
import {defineComponent} from 'vue'
import {invoke} from '@tauri-apps/api/core'
// import { open } from '@tauri-apps/plugin-shell' // если нет — убери openLogs() или используй opener plugin

export default defineComponent({
	name: 'App',
	data() {
		return {
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
		}
	},

	async created() {
		await this.bootstrap()
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
					await invoke('singbox_stop')
					this.isRunning = false
					return
				}

				// при старте требуем выбранный профиль
				if (!this.selectedProfile) {
					this.errorText = 'Выберите профиль'
					return
				}

				await invoke('singbox_start')
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
		}
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
</style>
