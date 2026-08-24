# V0.19.0 Restored

<div align="center">
  <img src="https://raw.githubusercontent.com/tw93/Kaku/main/assets/logo.png" alt="Kaku Logo" width="120" height="120" />
  <h1 style="margin: 12px 0 6px;">Kaku V0.19.0</h1>
  <p><em>A fast, out-of-the-box terminal built for AI coding.</em></p>
</div>

### Changelog

1. **Pane Input Broadcast Is Gone**: Typing in one pane no longer echoes into the others. It was too easy to trigger by accident and could repeat a risky command in panes you were not looking at. Old key assignments still load, they just do nothing.
2. **Sessions Come Back the Way You Left Them**: Reopening Kaku restores your windows, the panes inside them, and the directory each pane was in, even when one pane could not be captured on the way out.
3. **Closing Hits the Right Target**: A close confirmation stays tied to the pane it was opened for, and closing the active tab leaves you on the tab you expected.
4. **Display and Integration Fixes**: Selected rows stay visible in the light theme, lazygit opens correctly inside nested shells, clearing scrollback no longer disturbs a full-screen program, renaming a tab does not freeze its title, and slow synchronized output stops tearing.

### 更新日志

1. **移除分屏输入广播**：在一个分屏里打字不会再同步到别的分屏。它太容易误触，可能把有风险的命令在你没看着的分屏里重复一遍。原有快捷键仍然能加载，只是不再有动作。
2. **会话按你离开时的样子回来**：重新打开 Kaku，窗口、里面的分屏，以及每个分屏当时所在的目录都会恢复，其中某个分屏没能抓到快照也不影响其他的。
3. **关闭作用在正确的对象上**：关闭确认始终对应它被打开时的那个分屏，关掉当前标签页之后也会停在你预期的那个。
4. **显示与集成修复**：浅色主题下选中行不再看不见，lazygit 在嵌套 shell 里能正常打开，清空回滚不再打断全屏程序，重命名标签页不会冻住标题，慢速同步输出也不再撕裂。

Special thanks to @shlroland and @dufu1991 for their contributions to this release.

> https://github.com/tw93/Kaku
