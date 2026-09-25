//! 开机自启：把「本应用的 exe」挂到当前用户的登录启动项上。
//!
//! 用注册表而不是再引一个插件：winreg 本来就在依赖里，`platform::windows` 也一直在写注册表，
//! 这里沿用同一种做法。写 HKCU（当前用户）不需要管理员权限，也不会碰到别的用户的配置。
//!
//! 由设置里的「开机启动」驱动（默认开启）：应用启动时同步一次、开关变更时再同步一次。
//! 每次启动都重写一遍，顺带把「安装位置变了、注册表里还是旧路径」这种情况自愈掉。

#[cfg(windows)]
mod imp {
    use std::path::Path;

    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    use crate::error::{AppError, AppResult};

    /// 当前用户的登录启动项。
    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    /// 注册表里的值名，也是任务管理器「启动」页里显示的名字。
    const VALUE_NAME: &str = "AI Start";

    /// 启动命令行。路径带空格（`C:\Program Files\...`）必须加引号，
    /// 否则 Windows 会把第一个空格之前的部分当成可执行文件名。
    pub fn command_line(exe: &Path) -> String {
        format!("\"{}\"", exe.display())
    }

    pub fn apply(enabled: bool) -> AppResult<()> {
        // 用 `create_subkey` 而不是 `open_subkey`：万一 Run 项被清理工具删了，
        // 打开会失败，而这里本来就要建，顺手一起保证存在。
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
            .create_subkey(RUN_KEY)
            .map_err(|error| AppError::Message(format!("打开开机启动项失败: {error}")))?;

        if !enabled {
            return match key.delete_value(VALUE_NAME) {
                Ok(()) => Ok(()),
                // 本来就没写过，等同于「已经是关闭状态」。
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(AppError::Message(format!("关闭开机启动失败: {error}"))),
            };
        }

        let exe = std::env::current_exe()
            .map_err(|error| AppError::Message(format!("无法定位程序路径: {error}")))?;
        key.set_value(VALUE_NAME, &command_line(&exe))
            .map_err(|error| AppError::Message(format!("写入开机启动失败: {error}")))
    }
}

#[cfg(windows)]
pub use imp::apply;

/// 只给测试用：`apply` 会真的改注册表，测试只验证路径引号规则。
#[cfg(all(windows, test))]
pub use imp::command_line;

#[cfg(not(windows))]
pub fn apply(_enabled: bool) -> AppResult<()> {
    Err(AppError::Unsupported("开机启动目前仅支持 Windows".into()))
}
