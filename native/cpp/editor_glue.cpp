// Small C entry points the Rust side needs from C++: the Lexilla markdown lexer and the
// modern folder picker (IFileOpenDialog is COM, which is far simpler to drive from C++).

#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <shobjidl.h>

#include "ILexer.h"
#include "Scintilla.h"
#include "LexerModule.h"

extern const Lexilla::LexerModule lmMarkdown;

extern "C" void* md_create_lexer() {
	return lmMarkdown.Create();
}

/// Show the folder picker; writes the chosen path (NUL-terminated) and returns its length, or 0 when cancelled.
extern "C" int pick_folder(HWND owner, wchar_t* out, int cap) {
	IFileOpenDialog* dlg = nullptr;
	if (FAILED(CoCreateInstance(CLSID_FileOpenDialog, nullptr, CLSCTX_INPROC_SERVER, IID_PPV_ARGS(&dlg))))
		return 0;
	int len = 0;
	DWORD opts = 0;
	dlg->GetOptions(&opts);
	dlg->SetOptions(opts | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST);
	dlg->SetTitle(L"Open folder");
	if (SUCCEEDED(dlg->Show(owner))) {
		IShellItem* item = nullptr;
		if (SUCCEEDED(dlg->GetResult(&item))) {
			PWSTR path = nullptr;
			if (SUCCEEDED(item->GetDisplayName(SIGDN_FILESYSPATH, &path))) {
				len = lstrlenW(path);
				if (len < cap) {
					lstrcpynW(out, path, cap);
				} else {
					len = 0;
				}
				CoTaskMemFree(path);
			}
			item->Release();
		}
	}
	dlg->Release();
	return len;
}
