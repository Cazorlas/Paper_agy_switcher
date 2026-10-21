# paper-agy-switch

**Công cụ chuyển đổi nhiều tài khoản Google Antigravity, với bảng quota cho agy CLI và ứng dụng Antigravity desktop.**

[English](README.md)

Lưu nhiều phiên đăng nhập Google, xem gói và quota từng tài khoản, rồi chuyển bằng một lệnh. `agy` CLI và ứng dụng Antigravity 2.x desktop dùng chung một phiên đăng nhập trong Windows Credential Manager (`gemini:antigravity`); công cụ lưu nhiều phiên đăng nhập và thay thế mục này.

## Tính năng

- Lưu tài khoản, thêm tài khoản qua đăng nhập Google trên trình duyệt, đổi tên và khôi phục profile đã xóa.
- Bảng `list` có số thứ tự, căn cột và bảng điều khiển `tui` tương tác, hiển thị gói, phần trăm đã dùng và thời gian còn lại đến lúc reset.
- Quota Claude/GPT 5 giờ và hằng tuần, cùng quota Gemini 5 giờ và hằng tuần, từ `retrieveUserQuotaSummary` của Google Cloud Code.
- Chuyển theo bí danh, số thứ tự hoặc tài khoản tốt nhất trong nhóm chấm điểm đã chọn; khởi chạy `agy` hoặc dùng `auto` để tự động chuyển.

## Yêu cầu

Hiện chỉ hỗ trợ Windows. Chưa hỗ trợ macOS Keychain và Linux secret-service. Cài `agy` để đăng nhập và khởi chạy Antigravity.

## Cài đặt

```sh
npm i -g paper-agy-switch
```

Gói npm tải binary dựng sẵn từ [GitHub Releases](https://github.com/Cazorlas/Paper_agy_switcher/releases).

Hoặc dựng từ mã nguồn:

```sh
cargo install --git https://github.com/Cazorlas/Paper_agy_switcher
```

Dữ liệu nằm tại `~/.paper-agy-switch`; đặt `PAPER_AGY_SWITCH_HOME` để đổi thư mục này.

## Thiết lập lần đầu

1. Mở `agy`, chạy `/login` bên trong và đăng nhập tài khoản Google đầu tiên.
2. Lưu phiên đăng nhập đó:
   ```sh
   paper-agy-switch login main
   ```
3. Thêm tài khoản khác:
   ```sh
   paper-agy-switch add work
   ```
   Đăng nhập Google trên trình duyệt. Phiên đăng nhập được lưu thẳng vào profile: không cần đóng ứng dụng hay dùng `/logout`.
4. Kiểm tra tài khoản bằng `paper-agy-switch list`.

**Không dùng `/logout` trong agy cho tài khoản đã lưu:** thao tác này có thể thu hồi phiên đăng nhập đã lưu. Khi phiên đăng nhập hết hiệu lực, chạy `/login` trong agy, rồi chạy lại `paper-agy-switch login <alias>`.

## Sử dụng hằng ngày

```sh
paper-agy-switch list                  # bảng quota có số thứ tự
paper-agy-switch use main              # chuyển theo bí danh
paper-agy-switch use 2                 # chuyển theo số thứ tự trong list
paper-agy-switch use                   # tài khoản tốt nhất trong nhóm chấm điểm
paper-agy-switch launch work -- --continue # chuyển rồi mở agy với các tham số này
paper-agy-switch tui                   # bảng điều khiển và Settings
paper-agy-switch auto                  # tự động chuyển tài khoản
paper-agy-switch doctor                # kiểm tra thiết lập và phiên đăng nhập
```

Số thứ tự theo thứ tự bí danh trong `list`, khớp với `use <n>`. Nếu bí danh chính là một số, bí danh được ưu tiên hơn số dòng. `*` đánh dấu profile đang hoạt động; các ô như `↻2h13m` và `↻5d4h` hiển thị thời gian còn lại đến lúc reset, còn `--` nghĩa là không có dữ liệu. Lỗi lấy dữ liệu xuất hiện trong cột Status.

### Phiên đang chạy và việc ghi đè thông tin đăng nhập

Giới hạn đã đo thực tế: tiến trình `agy` đang chạy giữ tài khoản lúc khởi động. Điều này áp dụng cho terminal agy, Antigravity hub do VS Code khởi động và ứng dụng desktop. Khi tiến trình làm mới token (khoảng mỗi giờ), nó ghi tài khoản của mình trở lại Credential Manager, làm mất hiệu lực lần chuyển. Tiến trình agy mới dùng ngay tài khoản vừa chuyển.

`use` liệt kê các tiến trình agy đang chạy. Để dừng chúng khi chuyển:

```sh
paper-agy-switch use main --restart-agy
```

`auto` thông báo khi thông tin đăng nhập bị ghi đè.

### Nhóm chấm điểm

Chọn nhóm dùng để tìm tài khoản tốt nhất trong `~/.paper-agy-switch/config.toml` hoặc TUI Settings:

```toml
[use]
pool = "claude" # mặc định: nhóm Claude/GPT; dùng "gemini" cho Gemini
```

### Nguồn quota

Google tính quota Gemini riêng cho CLI `agy` và cho Antigravity IDE/ứng dụng desktop, nên hai nơi hiện số khác nhau. Mặc định công cụ hiện quota của **agy CLI**, giống con số `agy -p /usage` in ra. Đổi sang quota của IDE trong `~/.paper-agy-switch/config.toml` hoặc TUI Settings:

```toml
[quota]
source = "cli" # mặc định: quota của agy CLI; dùng "ide" cho quota của Antigravity IDE
```

`list` và tiêu đề TUI cho biết đang hiện quota nào (`quota: agy CLI` hoặc `quota: Antigravity IDE`), `list --json` thêm trường `quota_source` ở mỗi dòng, và `doctor` in ra giá trị đang đặt. Ghi chú giao thức nằm trong [docs/agy-protocol.md](docs/agy-protocol.md).

### Giới hạn của bảng quota

Quan sát một lần (2026-10-21, một account): agy từ chối Gemini 3.8 Flash với **"Individual quota reached ... Resets in 61h"** trong khi cả `retrieveUserQuotaSummary` lẫn `fetchAvailableModels` (theo từng model) đều báo model đó còn 100%, và các model Claude trên cùng account vẫn chạy bình thường. Google áp dụng những giới hạn mà các API này không trả về, nên bảng quota có thể hiện còn dư trong khi một model đã hết. Hãy thử model khác (`agy --model ...`) hoặc account khác.

## Quản lý tài khoản

| Thao tác | Lệnh |
|---|---|
| Lưu phiên đăng nhập agy hiện tại | `paper-agy-switch login [alias]` |
| Thêm qua đăng nhập trên trình duyệt | `paper-agy-switch add <alias>` |
| Làm mới quota, bỏ qua cache | `paper-agy-switch list --force` |
| Đổi tên | `paper-agy-switch rename <old> <new>` |
| Xóa, có thể khôi phục | `paper-agy-switch delete <alias-or-number> [--yes]` |
| Liệt kê profile đã xóa | `paper-agy-switch restore` |
| Khôi phục profile | `paper-agy-switch restore <alias> [--as <new>]` |

## Cập nhật / gỡ cài đặt

```sh
paper-agy-switch self-update --check
paper-agy-switch self-update
```

Nếu cài từ mã nguồn, chạy lại lệnh `cargo install --git` ở trên.

**Trình khởi chạy npm** cung cấp `uninstall`; đây không phải lệnh trong `--help` của binary:

```sh
paper-agy-switch uninstall             # hỏi có xóa dữ liệu đã lưu khi chạy trong terminal tương tác
paper-agy-switch uninstall --keep-data # giữ tài khoản và thiết lập
paper-agy-switch uninstall --purge     # xóa tài khoản và thiết lập
```

## Cách hoạt động

Công cụ lưu các phiên đăng nhập trong profile dưới thư mục dữ liệu. Khi chuyển, nó thay thế mục Windows Credential Manager dùng chung `gemini:antigravity`, là mục các tiến trình agy và Antigravity desktop mới đọc.

## Ghi nhận và giấy phép

Lấy cảm hứng từ [Cockpit Tools](https://github.com/jlcodes99/cockpit-tools) về nghiên cứu giao thức Antigravity (chỉ hành vi; không sao chép mã). Được fork từ [Paper.Claude-switcher](https://github.com/Cazorlas/Paper.Claude-switcher).

Apache-2.0 — xem [LICENSE](LICENSE).
