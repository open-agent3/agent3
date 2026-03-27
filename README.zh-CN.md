# Agent3

[English](README.md) | [中文](README.zh-CN.md)

[![CI](https://github.com/open-agent3/agent3/actions/workflows/ci.yml/badge.svg)](https://github.com/open-agent3/agent3/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

> 一个自然、快速、始终贴近你身边的系统级环境式 AI 语音助手。

🌐 **[访问官方网站与下载页面](https://open-agent3.github.io/agent3/)**

**当前状态**: Public Alpha (`v0.1.x`)

## 📥 下载与安装

对于普通用户，你无需配置任何代码环境。直接前往官网下载对应操作系统的安装包即可：

**[👉 获取 Agent3 (支持 Windows / macOS / Linux)](https://open-agent3.github.io/agent3/)**

> **注**：安装完成后，应用会在系统托盘运行。初次使用时，你需要自备并填入 API Key（OpenAI 或 Gemini）才能唤醒它。

---

## ✨ 人人都能上手

即使你不是技术用户，通常也只需要一个 API Key 就能开始使用。

Agent3 已经适合早期体验者、开发者和贡献者试用。核心语音工作流已经可用，但整个产品仍在快速演进中。

📱 **技术栈**: Tauri 2.0 + Vue 3 + TypeScript + Rust + SQLite + cpal  
🧠 **服务提供商**: OpenAI、Gemini（Realtime WebSockets）  
🚀 **功能特性**: 全原生音频处理、Rustpotter 唤醒词检测、透明桌面覆盖层、本地知识图谱记忆、系统级工具执行。

## ✅ 当前已可用

- 基于 OpenAI 和 Gemini 的实时语音对话
- Rust 原生音频采集与播放
- 唤醒词创建、激活与检测
- 音色切换、桌面覆盖层与托盘控制
- 设置、对话记录与记忆数据的本地持久化

## ⚠️ 当前边界

- 这还不是一个打磨完成的 1.0 消费级版本
- 在不同设备、麦克风和系统边缘场景下，表现仍可能存在差异
- 首次使用流程已经改善，但某些系统级交互仍需要继续优化
- 产品会快速迭代，期间可能出现行为变化

## 🔒 隐私说明

- 音频由本地原生应用采集
- 设置和记忆数据默认存储在本地 SQLite 中
- 当实时会话处于活动状态时，音频与消息会发送到你配置的 AI 服务提供商
- 使用前应查看你所选服务提供商的隐私条款

## ⚠️ 安全警告

- Agent3 可以执行系统级工具操作，包括 shell 命令和桌面动作
- 如果设备中包含高度敏感数据，在你未完全信任当前配置前，不应直接使用它
- 在日常使用前，请认真检查所选服务提供商、提示词以及后续工具权限设计
- 在早期测试阶段，更建议先在非关键环境或备用设备上体验

## ⚡ 环境要求

要构建并运行 Agent3，需要先安装以下依赖：
- [Rust](https://www.rust-lang.org/tools/install)（最新稳定版）
- [Node.js](https://nodejs.org/)（v18+）
- [pnpm](https://pnpm.io/installation)（v8+）
- Tauri 对应平台所需的系统构建依赖（见 [Tauri Prerequisites](https://tauri.app/v1/guides/getting-started/prerequisites)）

## 🛠️ 快速开始

```bash
# 1. 安装依赖
pnpm install

# 2. 启动开发模式（支持热重载）
pnpm tauri dev
```

> **注意**: 主窗口是一个透明、始终置顶且可点击穿透的图层。要进行配置，请在系统托盘中找到 Agent3 图标并打开 Settings。

## 🤝 参与贡献

欢迎社区贡献。请先阅读 [CONTRIBUTING.md](CONTRIBUTING.md)，了解行为准则、开发流程以及提交 Pull Request 的方式。  
**注意**: 所有 Pull Request 都应提交到 `dev` 分支。

## 📜 许可证

本项目基于 MIT License 开源，详情见 [LICENSE](LICENSE)。