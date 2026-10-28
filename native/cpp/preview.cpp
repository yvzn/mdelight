// Markdown preview control: litehtml lays out the comrak HTML, Direct2D/DirectWrite draw it.
//
// The Rust side creates the window with pv_create() and pushes documents with pv_set_document().
// The control talks back to its parent with messages (see PVN_* below).

#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <windowsx.h>
#include <d2d1.h>
#include <dwrite.h>
#include <wincodec.h>
#include <shellapi.h>
#include <uxtheme.h>

#include <algorithm>
#include <cmath>
#include <memory>
#include <string>
#include <unordered_map>
#include <vector>

#include "litehtml.h"
#include "render_item.h"

using namespace litehtml;

// Notifications sent to the parent window.
static const UINT PVN_SCROLL = WM_APP + 0x100;  // user scrolled the preview
static const UINT PVN_LINK = WM_APP + 0x101;    // lParam: const char* href (valid during the call)
static const UINT PVN_TOAST = WM_APP + 0x102;   // lParam: const char* message
static const UINT PVN_MERMAID = WM_APP + 0x103; // lParam: const char* diagram source (UTF-8)

struct PvAnchor {
	int line;
	float top;
};

template <class T> static void release(T*& p) {
	if (p) {
		p->Release();
		p = nullptr;
	}
}

static ID2D1Factory* g_d2d = nullptr;
static IDWriteFactory* g_dw = nullptr;
static IWICImagingFactory* g_wic = nullptr;
static IDWriteFontCollection* g_fonts = nullptr;

static void init_factories() {
	if (g_d2d)
		return;
	D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, &g_d2d);
	DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED, __uuidof(IDWriteFactory), reinterpret_cast<IUnknown**>(&g_dw));
	CoCreateInstance(CLSID_WICImagingFactory, nullptr, CLSCTX_INPROC_SERVER, IID_PPV_ARGS(&g_wic));
	g_dw->GetSystemFontCollection(&g_fonts);
}

static std::wstring widen(const char* s, int n = -1) {
	if (!s || !*s || n == 0)
		return {};
	int len = MultiByteToWideChar(CP_UTF8, 0, s, n, nullptr, 0);
	std::wstring w(len, L'\0');
	MultiByteToWideChar(CP_UTF8, 0, s, n, w.data(), len);
	if (n < 0 && !w.empty() && w.back() == L'\0')
		w.pop_back();
	return w;
}

static std::string narrow(const std::wstring& w) {
	if (w.empty())
		return {};
	int len = WideCharToMultiByte(CP_UTF8, 0, w.data(), (int)w.size(), nullptr, 0, nullptr, nullptr);
	std::string s(len, '\0');
	WideCharToMultiByte(CP_UTF8, 0, w.data(), (int)w.size(), s.data(), len, nullptr, nullptr);
	return s;
}

static D2D1_COLOR_F to_d2d(const web_color& c) {
	return D2D1::ColorF(c.red / 255.f, c.green / 255.f, c.blue / 255.f, c.alpha / 255.f);
}

static D2D1_RECT_F to_rect(const position& p) {
	return D2D1::RectF(p.x, p.y, p.x + p.width, p.y + p.height);
}

static void set_clipboard(HWND hwnd, const std::wstring& text) {
	if (!OpenClipboard(hwnd))
		return;
	EmptyClipboard();
	size_t bytes = (text.size() + 1) * sizeof(wchar_t);
	if (HGLOBAL mem = GlobalAlloc(GMEM_MOVEABLE, bytes)) {
		memcpy(GlobalLock(mem), text.c_str(), bytes);
		GlobalUnlock(mem);
		if (!SetClipboardData(CF_UNICODETEXT, mem))
			GlobalFree(mem);
	}
	CloseClipboard();
}

// ---------------------------------------------------------------- fonts

struct Font {
	IDWriteTextFormat* fmt = nullptr;
	float size = 0, ascent = 0, descent = 0;
	float ul_pos = 0, ul_thick = 1, st_pos = 0, st_thick = 1;
	int decoration = 0;
	web_color deco_color;
	std::unordered_map<std::string, float> widths;
};

/// Fonts outlive documents: the preview re-parses on every edit, and measuring is the slow part.
static std::unordered_map<std::string, std::unique_ptr<Font>> g_font_cache;

static bool family_exists(const std::wstring& name) {
	UINT32 idx;
	BOOL exists = FALSE;
	return SUCCEEDED(g_fonts->FindFamilyName(name.c_str(), &idx, &exists)) && exists;
}

static std::wstring pick_family(const std::string& families) {
	size_t start = 0;
	while (start <= families.size()) {
		size_t end = families.find(',', start);
		if (end == std::string::npos)
			end = families.size();
		std::string f = families.substr(start, end - start);
		f.erase(0, f.find_first_not_of(" \t\"'"));
		f.erase(f.find_last_not_of(" \t\"'") + 1);
		std::vector<std::wstring> candidates;
		if (f == "monospace")
			candidates = {L"Cascadia Mono", L"Consolas", L"Courier New"};
		else if (f == "sans-serif" || f == "system-ui")
			candidates = {L"Segoe UI Variable Text", L"Segoe UI"};
		else if (f == "serif")
			candidates = {L"Georgia", L"Times New Roman"};
		else if (!f.empty())
			candidates = {widen(f.c_str())};
		for (auto& c : candidates)
			if (family_exists(c))
				return c;
		start = end + 1;
	}
	return L"Segoe UI";
}

static float measure(Font* f, const char* text) {
	auto it = f->widths.find(text);
	if (it != f->widths.end())
		return it->second;
	std::wstring w = widen(text);
	float width = 0;
	IDWriteTextLayout* lay = nullptr;
	if (!w.empty() && SUCCEEDED(g_dw->CreateTextLayout(w.c_str(), (UINT32)w.size(), f->fmt, 100000.f, 1000.f, &lay))) {
		DWRITE_TEXT_METRICS tm;
		if (SUCCEEDED(lay->GetMetrics(&tm)))
			width = tm.widthIncludingTrailingWhitespace;
		// Italic glyphs lean past their advance; reserve the overhang so the next word doesn't touch.
		DWRITE_OVERHANG_METRICS om;
		if (f->fmt->GetFontStyle() != DWRITE_FONT_STYLE_NORMAL && SUCCEEDED(lay->GetOverhangMetrics(&om)) && om.right > 0)
			width += om.right;
		lay->Release();
	}
	if (f->widths.size() > 50000)
		f->widths.clear();
	f->widths.emplace(text, width);
	return width;
}

// ---------------------------------------------------------------- container

struct Image {
	IWICBitmapSource* src = nullptr;
	ID2D1Bitmap* bmp = nullptr;
	UINT w = 0, h = 0;
};

class Container : public document_container {
public:
	ID2D1RenderTarget* rt = nullptr;
	ID2D1SolidColorBrush* brush = nullptr;
	std::wstring base_dir;
	std::string cursor;
	std::string clicked;
	float vw = 800, vh = 600;
	int clip_depth = 0;
	std::unordered_map<std::wstring, Image> images;

	void set_brush(const web_color& c) { brush->SetColor(to_d2d(c)); }

	void drop_bitmaps() {
		for (auto& [_, img] : images)
			release(img.bmp);
	}

	/// Workspace-relative image path → absolute file path; empty for remote/data URLs.
	std::wstring resolve(const char* src) const {
		std::string s = src ? src : "";
		if (s.empty() || s.rfind("http:", 0) == 0 || s.rfind("https:", 0) == 0 || s.rfind("data:", 0) == 0 || s.rfind("//", 0) == 0)
			return {};
		s = s.substr(0, s.find_first_of("?#"));
		if (s.rfind("file:///", 0) == 0)
			s = s.substr(8);
		std::string dec;
		for (size_t i = 0; i < s.size(); i++) {
			if (s[i] == '%' && i + 2 < s.size() && isxdigit((unsigned char)s[i + 1]) && isxdigit((unsigned char)s[i + 2])) {
				dec += (char)strtol(s.substr(i + 1, 2).c_str(), nullptr, 16);
				i += 2;
			} else {
				dec += s[i] == '/' ? '\\' : s[i];
			}
		}
		std::wstring w = widen(dec.c_str());
		bool absolute = (w.size() > 2 && w[1] == L':') || w.rfind(L"\\\\", 0) == 0;
		return absolute ? w : base_dir + L"\\" + w;
	}

	Image* image(const char* src) {
		std::wstring path = resolve(src);
		if (path.empty())
			return nullptr;
		auto it = images.find(path);
		if (it != images.end())
			return it->second.src ? &it->second : nullptr;
		if (images.size() > 128) {
			for (auto& [_, img] : images) {
				release(img.bmp);
				release(img.src);
			}
			images.clear();
		}
		Image img;
		IWICBitmapDecoder* dec = nullptr;
		IWICBitmapFrameDecode* frame = nullptr;
		IWICFormatConverter* conv = nullptr;
		if (SUCCEEDED(g_wic->CreateDecoderFromFilename(path.c_str(), nullptr, GENERIC_READ, WICDecodeMetadataCacheOnLoad, &dec)) &&
			SUCCEEDED(dec->GetFrame(0, &frame)) && SUCCEEDED(g_wic->CreateFormatConverter(&conv)) &&
			SUCCEEDED(conv->Initialize(frame, GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, nullptr, 0, WICBitmapPaletteTypeMedianCut))) {
			conv->GetSize(&img.w, &img.h);
			img.src = conv;
			conv = nullptr;
		}
		release(conv);
		release(frame);
		release(dec);
		auto& slot = images[path] = img;
		return slot.src ? &slot : nullptr;
	}

	uint_ptr create_font(const font_description& d, const document*, font_metrics* fm) override {
		std::string key = d.hash();
		auto it = g_font_cache.find(key);
		Font* f;
		if (it != g_font_cache.end()) {
			f = it->second.get();
		} else {
			auto nf = std::make_unique<Font>();
			f = nf.get();
			std::wstring fam = pick_family(d.family);
			auto weight = (DWRITE_FONT_WEIGHT)std::clamp(d.weight, 100, 950);
			auto style = d.style == font_style_italic ? DWRITE_FONT_STYLE_ITALIC : DWRITE_FONT_STYLE_NORMAL;
			float size = d.size > 0 ? d.size : 16.f;
			g_dw->CreateTextFormat(fam.c_str(), nullptr, weight, style, DWRITE_FONT_STRETCH_NORMAL, size, L"en-us", &f->fmt);
			f->fmt->SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
			f->size = size;

			DWRITE_FONT_METRICS m{};
			UINT32 idx;
			BOOL exists = FALSE;
			IDWriteFontFamily* ff = nullptr;
			IDWriteFont* font = nullptr;
			if (SUCCEEDED(g_fonts->FindFamilyName(fam.c_str(), &idx, &exists)) && exists && SUCCEEDED(g_fonts->GetFontFamily(idx, &ff)) &&
				SUCCEEDED(ff->GetFirstMatchingFont(weight, DWRITE_FONT_STRETCH_NORMAL, style, &font))) {
				font->GetMetrics(&m);
			}
			release(font);
			release(ff);
			if (!m.designUnitsPerEm) {
				m.designUnitsPerEm = 2048;
				m.ascent = 2210;
				m.descent = 514;
				m.xHeight = 1024;
				m.underlinePosition = -300;
				m.underlineThickness = 100;
				m.strikethroughPosition = 600;
				m.strikethroughThickness = 100;
			}
			float s = size / m.designUnitsPerEm;
			f->ascent = std::round(m.ascent * s);
			f->descent = std::round(m.descent * s);
			f->ul_pos = m.underlinePosition * s;
			f->ul_thick = std::max(1.f, m.underlineThickness * s);
			f->st_pos = m.strikethroughPosition * s;
			f->st_thick = std::max(1.f, m.strikethroughThickness * s);
			f->fmt->SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, f->ascent + f->descent, f->ascent);
			f->decoration = d.decoration_line;
			f->deco_color = d.decoration_color;
			g_font_cache.emplace(key, std::move(nf));
			fm->x_height = m.xHeight * s;
		}
		if (fm) {
			fm->font_size = f->size;
			fm->ascent = f->ascent;
			fm->descent = f->descent;
			fm->height = f->ascent + f->descent;
			if (fm->x_height <= 0)
				fm->x_height = f->size * 0.5f;
			fm->ch_width = measure(f, "0");
			fm->draw_spaces = f->decoration != text_decoration_line_none;
			fm->sub_shift = f->size * 0.2f;
			fm->super_shift = f->size * 0.35f;
		}
		return (uint_ptr)f;
	}

	void delete_font(uint_ptr) override {}

	pixel_t text_width(const char* text, uint_ptr hFont) override { return measure((Font*)hFont, text); }

	void draw_text(uint_ptr hdc, const char* text, uint_ptr hFont, web_color color, const position& pos) override {
		auto rt = (ID2D1RenderTarget*)hdc;
		Font* f = (Font*)hFont;
		std::wstring w = widen(text);
		set_brush(color);
		if (!w.empty())
			rt->DrawText(w.c_str(), (UINT32)w.size(), f->fmt, D2D1::RectF(pos.x, pos.y, pos.x + pos.width + 1000.f, pos.y + pos.height + 100.f), brush,
						 D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
		if (f->decoration) {
			if (!f->deco_color.is_current_color)
				set_brush(f->deco_color);
			float base = pos.y + f->ascent;
			if (f->decoration & text_decoration_line_underline)
				rt->FillRectangle(D2D1::RectF(pos.x, base - f->ul_pos, pos.x + pos.width, base - f->ul_pos + f->ul_thick), brush);
			if (f->decoration & text_decoration_line_line_through)
				rt->FillRectangle(D2D1::RectF(pos.x, base - f->st_pos, pos.x + pos.width, base - f->st_pos + f->st_thick), brush);
			if (f->decoration & text_decoration_line_overline)
				rt->FillRectangle(D2D1::RectF(pos.x, pos.y, pos.x + pos.width, pos.y + f->ul_thick), brush);
		}
	}

	pixel_t pt_to_px(float pt) const override { return pt * 96.f / 72.f; }
	pixel_t get_default_font_size() const override { return 15; }
	const char* get_default_font_name() const override { return "Segoe UI"; }

	void draw_list_marker(uint_ptr hdc, const list_marker& m) override {
		auto rt = (ID2D1RenderTarget*)hdc;
		set_brush(m.color);
		float cx = m.pos.x + m.pos.width / 2, cy = m.pos.y + m.pos.height / 2;
		float r = std::min(m.pos.width, m.pos.height) / 2;
		switch (m.marker_type) {
		case list_style_type_circle:
			rt->DrawEllipse(D2D1::Ellipse(D2D1::Point2F(cx, cy), r - 0.5f, r - 0.5f), brush, 1.f);
			break;
		case list_style_type_disc:
			rt->FillEllipse(D2D1::Ellipse(D2D1::Point2F(cx, cy), r, r), brush);
			break;
		case list_style_type_square:
			rt->FillRectangle(to_rect(m.pos), brush);
			break;
		default:
			break;
		}
	}

	void load_image(const char* src, const char*, bool) override { image(src); }

	void get_image_size(const char* src, const char*, size& sz) override {
		if (Image* img = image(src)) {
			sz.width = (pixel_t)img->w;
			sz.height = (pixel_t)img->h;
		} else {
			sz.width = sz.height = 0;
		}
	}

	void draw_image(uint_ptr hdc, const background_layer& layer, const std::string& url, const std::string&) override {
		auto rt = (ID2D1RenderTarget*)hdc;
		Image* img = image(url.c_str());
		if (!img)
			return;
		if (!img->bmp && FAILED(rt->CreateBitmapFromWicBitmap(img->src, nullptr, &img->bmp)))
			return;
		rt->PushAxisAlignedClip(to_rect(layer.clip_box), D2D1_ANTIALIAS_MODE_ALIASED);
		rt->DrawBitmap(img->bmp, to_rect(layer.origin_box), 1.f, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR);
		rt->PopAxisAlignedClip();
	}

	void fill(ID2D1RenderTarget* rt, const background_layer& layer, const web_color& color) {
		if (color.alpha == 0 || layer.is_root)
			return;
		set_brush(color);
		rt->PushAxisAlignedClip(to_rect(layer.clip_box), D2D1_ANTIALIAS_MODE_ALIASED);
		float r = layer.border_radius.top_left_x;
		if (r > 0)
			rt->FillRoundedRectangle(D2D1::RoundedRect(to_rect(layer.border_box), r, r), brush);
		else
			rt->FillRectangle(to_rect(layer.border_box), brush);
		rt->PopAxisAlignedClip();
	}

	void draw_solid_fill(uint_ptr hdc, const background_layer& layer, const web_color& color) override {
		fill((ID2D1RenderTarget*)hdc, layer, color);
	}
	// Gradients are not used by the preview stylesheet; paint their first stop.
	void draw_linear_gradient(uint_ptr hdc, const background_layer& layer, const background_layer::linear_gradient& g) override {
		if (!g.color_points.empty())
			fill((ID2D1RenderTarget*)hdc, layer, g.color_points.front().color);
	}
	void draw_radial_gradient(uint_ptr hdc, const background_layer& layer, const background_layer::radial_gradient& g) override {
		if (!g.color_points.empty())
			fill((ID2D1RenderTarget*)hdc, layer, g.color_points.front().color);
	}
	void draw_conic_gradient(uint_ptr hdc, const background_layer& layer, const background_layer::conic_gradient& g) override {
		if (!g.color_points.empty())
			fill((ID2D1RenderTarget*)hdc, layer, g.color_points.front().color);
	}

	static bool visible(const border& b) { return b.width > 0 && b.style > border_style_hidden && b.color.alpha > 0; }

	void draw_borders(uint_ptr hdc, const borders& b, const position& p, bool) override {
		auto rt = (ID2D1RenderTarget*)hdc;
		bool uniform = b.left.width == b.top.width && b.left.width == b.right.width && b.left.width == b.bottom.width &&
					   b.left.color == b.top.color && b.left.color == b.right.color && b.left.color == b.bottom.color &&
					   visible(b.left) && visible(b.top) && visible(b.right) && visible(b.bottom);
		float r = b.radius.top_left_x;
		if (uniform && r > 0) {
			float w = b.left.width;
			set_brush(b.left.color);
			D2D1_RECT_F rc = D2D1::RectF(p.x + w / 2, p.y + w / 2, p.right() - w / 2, p.bottom() - w / 2);
			rt->DrawRoundedRectangle(D2D1::RoundedRect(rc, r, r), brush, w);
			return;
		}
		if (visible(b.top)) {
			set_brush(b.top.color);
			rt->FillRectangle(D2D1::RectF(p.x, p.y, p.right(), p.y + b.top.width), brush);
		}
		if (visible(b.bottom)) {
			set_brush(b.bottom.color);
			rt->FillRectangle(D2D1::RectF(p.x, p.bottom() - b.bottom.width, p.right(), p.bottom()), brush);
		}
		if (visible(b.left)) {
			set_brush(b.left.color);
			rt->FillRectangle(D2D1::RectF(p.x, p.y, p.x + b.left.width, p.bottom()), brush);
		}
		if (visible(b.right)) {
			set_brush(b.right.color);
			rt->FillRectangle(D2D1::RectF(p.right() - b.right.width, p.y, p.right(), p.bottom()), brush);
		}
	}

	void set_caption(const char*) override {}
	void set_base_url(const char*) override {}
	void link(const std::shared_ptr<document>&, const element::ptr&) override {}
	void on_anchor_click(const char* url, const element::ptr&) override { clicked = url ? url : ""; }
	void on_mouse_event(const element::ptr&, litehtml::mouse_event) override {}
	void set_cursor(const char* c) override { cursor = c ? c : ""; }

	void transform_text(string& text, text_transform tt) override {
		if (tt != text_transform_uppercase && tt != text_transform_lowercase)
			return;
		std::wstring w = widen(text.c_str());
		if (tt == text_transform_uppercase)
			CharUpperBuffW(w.data(), (DWORD)w.size());
		else
			CharLowerBuffW(w.data(), (DWORD)w.size());
		text = narrow(w);
	}

	/// litehtml only wraps at spaces (no overflow-wrap): let long words like bare URLs also wrap
	/// after their punctuation, by splitting them into adjacent words with no space between.
	void split_text(const char* text, const std::function<void(const char*)>& on_word,
		const std::function<void(const char*)>& on_space) override {
		auto split_long = [&](const char* word) {
			const char* breaks = "/?&=#.-_";
			size_t n = strlen(word);
			if (n <= 24)
				return on_word(word);
			std::string piece;
			for (size_t i = 0; i < n; i++) {
				piece += word[i];
				if (strchr(breaks, word[i]) && i + 1 < n && !strchr(breaks, word[i + 1])) {
					on_word(piece.c_str());
					piece.clear();
				}
			}
			if (!piece.empty())
				on_word(piece.c_str());
		};
		document_container::split_text(text, split_long, on_space);
	}

	void import_css(string&, const string&, string&) override {}

	void set_clip(const position& pos, const border_radiuses&) override {
		rt->PushAxisAlignedClip(to_rect(pos), D2D1_ANTIALIAS_MODE_ALIASED);
		clip_depth++;
	}
	void del_clip() override {
		if (clip_depth > 0) {
			rt->PopAxisAlignedClip();
			clip_depth--;
		}
	}

	void get_viewport(position& v) const override { v = position(0, 0, vw, vh); }

	element::ptr create_element(const char*, const string_map&, const std::shared_ptr<document>&) override { return nullptr; }

	void get_media_features(media_features& m) const override {
		m.type = media_type_screen;
		m.width = vw;
		m.height = vh;
		m.device_width = (pixel_t)GetSystemMetrics(SM_CXSCREEN);
		m.device_height = (pixel_t)GetSystemMetrics(SM_CYSCREEN);
		m.color = 8;
		m.color_index = 256;
		m.monochrome = 0;
		m.resolution = 96;
	}

	void get_language(string& language, string& culture) const override {
		language = "en";
		culture = "";
	}
};

// ---------------------------------------------------------------- window

/// One word (or space) of text with its box in document coordinates, in reading order.
struct TextItem {
	element::ptr el;
	position box;
};

struct Preview {
	HWND hwnd = nullptr;
	ID2D1HwndRenderTarget* hrt = nullptr;
	Container c;
	document::ptr doc;
	float sx = 0, sy = 0;
	float layout_w = -1;
	float wheel_acc = 0;
	D2D1_COLOR_F bg = D2D1::ColorF(1, 1, 1);
	D2D1_COLOR_F sel = D2D1::ColorF(0.0f, 0.4f, 0.75f, 0.3f);
	std::string html, css;
	// Selection, by word: [sel_a, sel_b) caret indexes into `items`.
	std::vector<TextItem> items;
	bool items_dirty = true;
	int sel_a = -1, sel_b = -1;
	bool dragging = false, selecting = false;
	POINT down_pt{};
	// Hover "Copy" button on the code block under the mouse, plus "View" on Mermaid blocks.
	element::ptr hover_pre;
	position hover_box;  // document coordinates
	bool hover_mermaid = false;
	bool over_btn = false, over_view = false, copied = false;

	float scale() const { return GetDpiForWindow(hwnd) / 96.f; }
	float content_h() const { return doc ? doc->height() + c.vh * 0.4f : 0; }
	float content_w() const { return doc ? std::max(doc->width(), c.vw) : 0; }
};

static Preview* get(HWND h) { return reinterpret_cast<Preview*>(GetWindowLongPtrW(h, GWLP_USERDATA)); }

static void update_viewport(Preview* p) {
	RECT rc;
	GetClientRect(p->hwnd, &rc);
	float s = p->scale();
	p->c.vw = std::max(1.f, rc.right / s);
	p->c.vh = std::max(1.f, rc.bottom / s);
}

static void update_scrollbars(Preview* p) {
	float s = p->scale();
	SCROLLINFO si{sizeof(si), SIF_RANGE | SIF_PAGE | SIF_POS | SIF_DISABLENOSCROLL};
	si.nMin = 0;
	si.nMax = (int)std::ceil(p->content_h() * s);
	si.nPage = (UINT)std::floor(p->c.vh * s) + 1;
	si.nPos = (int)std::round(p->sy * s);
	SetScrollInfo(p->hwnd, SB_VERT, &si, TRUE);

	bool wide = p->doc && p->doc->width() > p->c.vw + 1;
	ShowScrollBar(p->hwnd, SB_HORZ, wide);
	if (wide) {
		SCROLLINFO hi{sizeof(hi), SIF_RANGE | SIF_PAGE | SIF_POS};
		hi.nMax = (int)std::ceil(p->content_w() * s);
		hi.nPage = (UINT)std::floor(p->c.vw * s) + 1;
		hi.nPos = (int)std::round(p->sx * s);
		SetScrollInfo(p->hwnd, SB_HORZ, &hi, TRUE);
	} else {
		p->sx = 0;
	}
}

static void layout(Preview* p, bool force) {
	update_viewport(p);
	if (!p->doc)
		return;
	if (force || std::fabs(p->layout_w - p->c.vw) > 0.5f) {
		p->doc->media_changed();
		p->doc->render(p->c.vw);
		p->layout_w = p->c.vw;
		p->items_dirty = true;
	}
	p->sy = std::clamp(p->sy, 0.f, std::max(0.f, p->content_h() - p->c.vh));
	update_scrollbars(p);
}

static void clear_hover(Preview* p) {
	if (p->hover_pre)
		InvalidateRect(p->hwnd, nullptr, FALSE);
	p->hover_pre.reset();
	p->hover_mermaid = p->over_btn = p->over_view = p->copied = false;
}

static void scroll_to(Preview* p, float x, float y, bool notify) {
	float maxy = std::max(0.f, p->content_h() - p->c.vh);
	float maxx = std::max(0.f, p->content_w() - p->c.vw);
	y = std::clamp(y, 0.f, maxy);
	x = std::clamp(x, 0.f, maxx);
	if (y == p->sy && x == p->sx)
		return;
	p->sy = y;
	p->sx = x;
	clear_hover(p);  // the block under the mouse changed; the next mouse move finds it again
	update_scrollbars(p);
	InvalidateRect(p->hwnd, nullptr, FALSE);
	if (notify)
		SendMessageW(GetParent(p->hwnd), PVN_SCROLL, 0, 0);
}

static void discard_target(Preview* p) {
	p->c.drop_bitmaps();
	release(p->c.brush);
	release(p->hrt);
	p->c.rt = nullptr;
}

static bool ensure_target(Preview* p) {
	if (p->hrt)
		return true;
	RECT rc;
	GetClientRect(p->hwnd, &rc);
	float dpi = (float)GetDpiForWindow(p->hwnd);
	auto props = D2D1::RenderTargetProperties(D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1::PixelFormat(), dpi, dpi);
	auto hprops = D2D1::HwndRenderTargetProperties(p->hwnd, D2D1::SizeU(rc.right, rc.bottom));
	if (FAILED(g_d2d->CreateHwndRenderTarget(props, hprops, &p->hrt)))
		return false;
	p->hrt->SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE);
	p->hrt->CreateSolidColorBrush(D2D1::ColorF(0, 0, 0), &p->c.brush);
	p->c.rt = p->hrt;
	return true;
}

static void collect_text(const element::ptr& el, std::vector<TextItem>& out) {
	if (el->is_text()) {
		if (auto ri = el->get_render_item())
			out.push_back({el, ri->get_placement()});
		return;
	}
	for (auto& ch : el->children())
		collect_text(ch, out);
}

static std::vector<TextItem>& text_items(Preview* p) {
	if (p->items_dirty) {
		p->items.clear();
		if (p->doc)
			collect_text(p->doc->root(), p->items);
		p->items_dirty = false;
	}
	return p->items;
}

/// Caret index for a point in document coordinates: how many words come before it in reading order.
static int caret_at(Preview* p, float x, float y) {
	auto& items = text_items(p);
	int n = 0;
	for (int i = 0; i < (int)items.size(); i++) {
		const auto& b = items[i].box;
		bool before = b.bottom() <= y || (b.y <= y && b.x + b.width / 2 <= x);
		if (before)
			n = i + 1;
	}
	return n;
}

static std::wstring selected_text(Preview* p) {
	auto& items = text_items(p);
	int a = std::min(p->sel_a, p->sel_b), b = std::max(p->sel_a, p->sel_b);
	std::string out;
	float prev_bottom = -1, prev_top = -1;
	for (int i = std::max(0, a); i < std::min(b, (int)items.size()); i++) {
		const auto& it = items[i];
		// A word split across lines (a wrapped URL) shares its parent with the piece before it.
		bool same_word = i > std::max(0, a) &&!it.el->is_white_space() && !items[i - 1].el->is_white_space() &&
			it.el->parent() == items[i - 1].el->parent();
		if (prev_top >= 0 && it.box.y >= prev_bottom - 1 && !same_word) {
			while (!out.empty() && out.back() == ' ')
				out.pop_back();
			out += "\r\n";
		}
		std::string t;
		it.el->get_text(t);
		if (out.empty() || out.back() != '\n' || t != " ")
			out += t;
		prev_top = it.box.y;
		prev_bottom = it.box.bottom();
	}
	return widen(out.c_str());
}

/// The Copy button of the hovered code block, in view coordinates; it sticks to the top of the
/// visible part of the block while scrolling through a long one.
static D2D1_RECT_F copy_button_rect(Preview* p) {
	const float w = 60, h = 26, m = 8;
	const position& b = p->hover_box;
	float right = b.right() - p->sx - m;
	float top = std::max(b.y - p->sy + m, m);
	top = std::min(top, b.bottom() - p->sy - h - m);
	top = std::max(top, b.y - p->sy + std::min(m, (b.height - h) / 2));
	return D2D1::RectF(right - w, top, right, top + h);
}

/// The View button of a hovered Mermaid block, left of Copy.
static D2D1_RECT_F view_button_rect(Preview* p) {
	D2D1_RECT_F r = copy_button_rect(p);
	float w = r.right - r.left + 6;
	return D2D1::RectF(r.left - w, r.top, r.right - w, r.bottom);
}

static void draw_button(Preview* p, ID2D1RenderTarget* rt, D2D1_RECT_F r, const wchar_t* label, bool hot, bool done) {
	static IDWriteTextFormat* fmt = nullptr;
	if (!fmt) {
		g_dw->CreateTextFormat(pick_family("sans-serif").c_str(), nullptr, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_FONT_STYLE_NORMAL,
							   DWRITE_FONT_STRETCH_NORMAL, 12.f, L"en-us", &fmt);
		fmt->SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
		fmt->SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
	}
	bool dark = p->bg.r * 0.3f + p->bg.g * 0.59f + p->bg.b * 0.11f < 0.5f;
	auto brush = p->c.brush;
	float a = hot ? 0.16f : 0.09f;
	brush->SetColor(dark ? D2D1::ColorF(1, 1, 1, a) : D2D1::ColorF(0, 0, 0, a));
	rt->FillRoundedRectangle(D2D1::RoundedRect(r, 5, 5), brush);
	D2D1_COLOR_F fg = dark ? D2D1::ColorF(0.85f, 0.86f, 0.88f) : D2D1::ColorF(0.2f, 0.22f, 0.25f);
	if (done)
		fg = D2D1::ColorF(p->sel.r, p->sel.g, p->sel.b);
	brush->SetColor(fg);
	rt->DrawText(label, (UINT32)wcslen(label), fmt, r, brush);
}

static void draw_doc(Preview* p, ID2D1RenderTarget* rt) {
	rt->SetTransform(D2D1::Matrix3x2F::Identity());
	rt->Clear(p->bg);
	if (p->doc) {
			position clip(0, 0, p->c.vw, p->c.vh);
			p->c.clip_depth = 0;
			p->doc->draw((uint_ptr)rt, -p->sx, -p->sy, &clip);
			while (p->c.clip_depth > 0)
				p->c.del_clip();
			if (p->sel_a >= 0 && p->sel_a != p->sel_b) {
				auto& items = text_items(p);
				int a = std::min(p->sel_a, p->sel_b), b = std::min(std::max(p->sel_a, p->sel_b), (int)items.size());
				p->c.brush->SetColor(p->sel);
				for (int i = a; i < b; i++) {
					auto r = items[i].box;
					if (r.bottom() < p->sy || r.y > p->sy + p->c.vh)
						continue;
					rt->FillRectangle(D2D1::RectF(r.x - p->sx, r.y - p->sy, r.right() - p->sx, r.bottom() - p->sy), p->c.brush);
				}
			}
			if (p->hover_pre && !p->selecting) {
				draw_button(p, rt, copy_button_rect(p), p->copied ? L"Copied" : L"Copy", p->over_btn, p->copied);
				if (p->hover_mermaid)
					draw_button(p, rt, view_button_rect(p), L"View", p->over_view, false);
			}
	}
}

static void paint(Preview* p) {
	PAINTSTRUCT ps;
	BeginPaint(p->hwnd, &ps);
	if (ensure_target(p)) {
		p->hrt->BeginDraw();
		draw_doc(p, p->hrt);
		if (p->hrt->EndDraw() == D2DERR_RECREATE_TARGET)
			discard_target(p);
	}
	EndPaint(p->hwnd, &ps);
}

/// Render the whole document into a PNG (used for automated checks; no window capture needed).
extern "C" int pv_snapshot(HWND h, const wchar_t* path) {
	Preview* p = get(h);
	if (!p)
		return 0;
	float dpi = (float)GetDpiForWindow(h);
	float s = dpi / 96.f;
	// Lay the view out as one tall page, then put the real viewport back afterwards.
	float saved_vh = p->c.vh, saved_sy = p->sy;
	p->c.vh = p->doc ? std::min(p->doc->height(), 6000.f) : p->c.vh;
	p->sy = 0;
	UINT w = std::max<UINT>((UINT)(p->c.vw * s), 1), hh = std::max<UINT>((UINT)(p->c.vh * s), 1);
	struct Restore {
		Preview* p;
		float vh, sy;
		~Restore() {
			p->c.vh = vh;
			p->sy = sy;
		}
	} restore{p, saved_vh, saved_sy};
	IWICBitmap* bmp = nullptr;
	ID2D1RenderTarget* rt = nullptr;
	int ok = 0;
	if (SUCCEEDED(g_wic->CreateBitmap(w, hh, GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnLoad, &bmp)) &&
		SUCCEEDED(g_d2d->CreateWicBitmapRenderTarget(bmp, D2D1::RenderTargetProperties(D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1::PixelFormat(DXGI_FORMAT_B8G8R8A8_UNORM, D2D1_ALPHA_MODE_PREMULTIPLIED), dpi, dpi), &rt))) {
		// Borrow the container for this target; bitmaps belong to one target, so drop them around it.
		auto saved_rt = p->c.rt;
		auto saved_brush = p->c.brush;
		p->c.drop_bitmaps();
		rt->CreateSolidColorBrush(D2D1::ColorF(0, 0, 0), &p->c.brush);
		p->c.rt = rt;
		rt->BeginDraw();
		draw_doc(p, rt);
		ok = SUCCEEDED(rt->EndDraw());
		release(p->c.brush);
		p->c.drop_bitmaps();
		p->c.rt = saved_rt;
		p->c.brush = saved_brush;

		IWICStream* stream = nullptr;
		IWICBitmapEncoder* enc = nullptr;
		IWICBitmapFrameEncode* frame = nullptr;
		if (ok && SUCCEEDED(g_wic->CreateStream(&stream)) && SUCCEEDED(stream->InitializeFromFilename(path, GENERIC_WRITE)) &&
			SUCCEEDED(g_wic->CreateEncoder(GUID_ContainerFormatPng, nullptr, &enc)) && SUCCEEDED(enc->Initialize(stream, WICBitmapEncoderNoCache)) &&
			SUCCEEDED(enc->CreateNewFrame(&frame, nullptr)) && SUCCEEDED(frame->Initialize(nullptr))) {
			frame->SetSize(w, hh);
			WICPixelFormatGUID fmt = GUID_WICPixelFormat32bppPBGRA;
			frame->SetPixelFormat(&fmt);
			frame->WriteSource(bmp, nullptr);
			frame->Commit();
			enc->Commit();
		} else {
			ok = 0;
		}
		release(frame);
		release(enc);
		release(stream);
	}
	release(rt);
	release(bmp);
	return ok;
}

static POINT client_pt(LPARAM lp) { return {GET_X_LPARAM(lp), GET_Y_LPARAM(lp)}; }

static void invalidate_boxes(Preview* p, const position::vector& boxes) {
	if (!boxes.empty())
		InvalidateRect(p->hwnd, nullptr, FALSE);
}

static element::ptr element_at(Preview* p, POINT pt) {
	if (!p->doc || !p->doc->root_render())
		return nullptr;
	float s = p->scale();
	float x = pt.x / s, y = pt.y / s;
	return p->doc->root_render()->get_element_by_point(x + p->sx, y + p->sy, x, y, nullptr);
}

static element::ptr ancestor(element::ptr el, std::initializer_list<const char*> tags) {
	for (; el; el = el->parent()) {
		if (el->is_text())
			continue;
		const char* t = el->get_tagName();
		for (const char* want : tags)
			if (t && _stricmp(t, want) == 0)
				return el;
	}
	return nullptr;
}

static std::wstring element_text(const element::ptr& el) {
	std::string t;
	el->get_text(t);
	return widen(t.c_str());
}

static void copy_code(Preview* p, const element::ptr& pre) {
	std::wstring t = element_text(pre);
	while (!t.empty() && t.back() == L'\n')
		t.pop_back();
	set_clipboard(p->hwnd, t);
}

/// A ```mermaid block: comrak puts the language on the <code> inside.
static bool is_mermaid(const element::ptr& pre) {
	for (auto& ch : pre->children()) {
		const char* t = ch->get_tagName();
		const char* cls = t && _stricmp(t, "code") == 0 ? ch->get_attr("class") : nullptr;
		if (cls && strstr(cls, "language-mermaid"))
			return true;
	}
	return false;
}

static void open_mermaid(Preview* p, const element::ptr& pre) {
	std::string t;
	pre->get_text(t);
	SendMessageW(GetParent(p->hwnd), PVN_MERMAID, 0, (LPARAM)t.c_str());
}

/// Track the code block under the mouse (and whether the pointer is on its Copy or View button).
static void update_hover(Preview* p, POINT pt) {
	auto pre = p->selecting ? nullptr : ancestor(element_at(p, pt), {"pre"});	if (pre != p->hover_pre) {
		clear_hover(p);
		auto ri = pre ? pre->get_render_item() : nullptr;
		if (ri) {
			p->hover_pre = pre;
			p->hover_mermaid = is_mermaid(pre);
			p->hover_box = ri->get_placement();
			p->hover_box += ri->get_paddings();
			p->hover_box += ri->get_borders();
		}
		InvalidateRect(p->hwnd, nullptr, FALSE);
	}
	float s = p->scale(), x = pt.x / s, y = pt.y / s;
	auto inside = [&](D2D1_RECT_F r) { return x >= r.left && x < r.right && y >= r.top && y < r.bottom; };
	bool over = p->hover_pre && inside(copy_button_rect(p));
	bool over_view = p->hover_mermaid && inside(view_button_rect(p));
	if (over != p->over_btn || over_view != p->over_view) {
		p->over_btn = over;
		p->over_view = over_view;
		InvalidateRect(p->hwnd, nullptr, FALSE);
	}
}

static void open_image(Preview* p, const element::ptr& img) {
	std::wstring path = p->c.resolve(img->get_attr("src", ""));
	if (!path.empty())
		ShellExecuteW(p->hwnd, L"open", path.c_str(), nullptr, nullptr, SW_SHOWNORMAL);
}

static void context_menu(Preview* p, POINT screen) {
	POINT pt = screen;
	if (screen.x == -1 && screen.y == -1) {
		pt = {10, 10};
		ClientToScreen(p->hwnd, &screen);
		screen = {screen.x + 10, screen.y + 10};
	} else {
		ScreenToClient(p->hwnd, &pt);
	}
	element::ptr el = element_at(p, pt);
	auto pre = ancestor(el, {"pre"});
	auto a = ancestor(el, {"a"});
	auto img = ancestor(el, {"img"});
	auto block = ancestor(el, {"p", "li", "td", "th", "h1", "h2", "h3", "h4", "h5", "h6", "blockquote", "pre"});
	bool has_sel = p->sel_a >= 0 && p->sel_a != p->sel_b;

	enum { CopySel = 1, CopyCode, ViewDiagram, CopyLink, OpenImage, CopyBlock, CopyAll, SelectAll };
	HMENU m = CreatePopupMenu();
	if (has_sel)
		AppendMenuW(m, MF_STRING, CopySel, L"Copy\tCtrl+C");
	if (pre)
		AppendMenuW(m, MF_STRING, CopyCode, L"Copy code");
	if (pre && is_mermaid(pre))
		AppendMenuW(m, MF_STRING, ViewDiagram, L"View diagram");
	if (a && a->get_attr("href"))
		AppendMenuW(m, MF_STRING, CopyLink, L"Copy link address");
	if (img)
		AppendMenuW(m, MF_STRING, OpenImage, L"Open image");
	if (block && block != pre)
		AppendMenuW(m, MF_STRING, CopyBlock, L"Copy paragraph");
	if (GetMenuItemCount(m))
		AppendMenuW(m, MF_SEPARATOR, 0, nullptr);
	AppendMenuW(m, MF_STRING, SelectAll, L"Select all\tCtrl+A");
	AppendMenuW(m, MF_STRING, CopyAll, L"Copy all text");
	int cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON, screen.x, screen.y, 0, p->hwnd, nullptr);
	DestroyMenu(m);
	switch (cmd) {
	case CopySel:
		set_clipboard(p->hwnd, selected_text(p));
		break;
	case CopyCode:
		copy_code(p, pre);
		SendMessageW(GetParent(p->hwnd), PVN_TOAST, 0, (LPARAM) "Code copied");
		break;
	case ViewDiagram:
		open_mermaid(p, pre);
		break;
	case CopyLink:
		set_clipboard(p->hwnd, widen(a->get_attr("href")));
		break;
	case OpenImage:
		open_image(p, img);
		break;
	case CopyBlock:
		set_clipboard(p->hwnd, element_text(block));
		break;
	case SelectAll:
		p->sel_a = 0;
		p->sel_b = (int)text_items(p).size();
		InvalidateRect(p->hwnd, nullptr, FALSE);
		break;
	case CopyAll:
		if (p->doc)
			set_clipboard(p->hwnd, element_text(p->doc->root()));
		break;
	}
}

static LRESULT CALLBACK preview_proc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp) {
	Preview* p = get(hwnd);
	switch (msg) {
	case WM_NCCREATE: {
		auto np = new Preview();
		np->hwnd = hwnd;
		SetWindowLongPtrW(hwnd, GWLP_USERDATA, (LONG_PTR)np);
		break;
	}
	case WM_NCDESTROY:
		if (p) {
			discard_target(p);
			for (auto& [_, img] : p->c.images)
				release(img.src);
			delete p;
			SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
		}
		break;
	case WM_ERASEBKGND:
		return 1;
	case WM_PAINT:
		paint(p);
		return 0;
	case WM_PRINTCLIENT: {
		// PrintWindow & co.: draw into the caller's DC through a DC render target.
		RECT rc;
		GetClientRect(hwnd, &rc);
		ID2D1DCRenderTarget* dcrt = nullptr;
		float dpi = (float)GetDpiForWindow(hwnd);
		auto props = D2D1::RenderTargetProperties(D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1::PixelFormat(DXGI_FORMAT_B8G8R8A8_UNORM, D2D1_ALPHA_MODE_IGNORE), dpi, dpi);
		if (SUCCEEDED(g_d2d->CreateDCRenderTarget(&props, &dcrt)) && SUCCEEDED(dcrt->BindDC((HDC)wp, &rc))) {
			auto saved_rt = p->c.rt;
			auto saved_brush = p->c.brush;
			p->c.drop_bitmaps();
			dcrt->CreateSolidColorBrush(D2D1::ColorF(0, 0, 0), &p->c.brush);
			p->c.rt = dcrt;
			dcrt->BeginDraw();
			draw_doc(p, dcrt);
			dcrt->EndDraw();
			release(p->c.brush);
			p->c.drop_bitmaps();
			p->c.rt = saved_rt;
			p->c.brush = saved_brush;
		}
		release(dcrt);
		return 0;
	}
	case WM_SIZE:
		if (p->hrt) {
			RECT rc;
			GetClientRect(hwnd, &rc);
			if (p->hrt->Resize(D2D1::SizeU(rc.right, rc.bottom)) != S_OK)
				discard_target(p);
		}
		layout(p, false);
		InvalidateRect(hwnd, nullptr, FALSE);
		return 0;
	case WM_DPICHANGED_AFTERPARENT:
		discard_target(p);
		layout(p, true);
		InvalidateRect(hwnd, nullptr, FALSE);
		return 0;
	case WM_GETDLGCODE:
		return DLGC_WANTARROWS;
	case WM_MOUSEWHEEL: {
		UINT lines = 3;
		SystemParametersInfoW(SPI_GETWHEELSCROLLLINES, 0, &lines, 0);
		p->wheel_acc += GET_WHEEL_DELTA_WPARAM(wp);
		float dy = -p->wheel_acc / WHEEL_DELTA * lines * 20.f;
		p->wheel_acc = 0;
		if (GET_KEYSTATE_WPARAM(wp) & MK_SHIFT)
			scroll_to(p, p->sx + dy, p->sy, true);
		else
			scroll_to(p, p->sx, p->sy + dy, true);
		return 0;
	}
	case WM_MOUSEHWHEEL:
		scroll_to(p, p->sx + GET_WHEEL_DELTA_WPARAM(wp) / (float)WHEEL_DELTA * 60.f, p->sy, true);
		return 0;
	case WM_VSCROLL:
	case WM_HSCROLL: {
		bool v = msg == WM_VSCROLL;
		float cur = v ? p->sy : p->sx, page = v ? p->c.vh : p->c.vw;
		float to = cur;
		switch (LOWORD(wp)) {
		case SB_LINEUP: to -= 40; break;
		case SB_LINEDOWN: to += 40; break;
		case SB_PAGEUP: to -= page * 0.9f; break;
		case SB_PAGEDOWN: to += page * 0.9f; break;
		case SB_TOP: to = 0; break;
		case SB_BOTTOM: to = 1e9f; break;
		case SB_THUMBTRACK:
		case SB_THUMBPOSITION: {
			SCROLLINFO si{sizeof(si), SIF_TRACKPOS};
			GetScrollInfo(hwnd, v ? SB_VERT : SB_HORZ, &si);
			to = si.nTrackPos / p->scale();
			break;
		}
		}
		if (v)
			scroll_to(p, p->sx, to, true);
		else
			scroll_to(p, to, p->sy, true);
		return 0;
	}
	case WM_KEYDOWN: {
		bool ctrl = GetKeyState(VK_CONTROL) < 0;
		switch (wp) {
		case VK_UP: scroll_to(p, p->sx, p->sy - 40, true); return 0;
		case VK_DOWN: scroll_to(p, p->sx, p->sy + 40, true); return 0;
		case VK_LEFT: scroll_to(p, p->sx - 40, p->sy, true); return 0;
		case VK_RIGHT: scroll_to(p, p->sx + 40, p->sy, true); return 0;
		case VK_PRIOR: scroll_to(p, p->sx, p->sy - p->c.vh * 0.9f, true); return 0;
		case VK_NEXT:
		case VK_SPACE: scroll_to(p, p->sx, p->sy + (GetKeyState(VK_SHIFT) < 0 ? -1 : 1) * p->c.vh * 0.9f, true); return 0;
		case VK_HOME: scroll_to(p, 0, 0, true); return 0;
		case VK_END: scroll_to(p, p->sx, 1e9f, true); return 0;
		case 'C':
			if (ctrl && p->sel_a >= 0 && p->sel_a != p->sel_b)
				set_clipboard(hwnd, selected_text(p));
			return 0;
		case 'A':
			if (ctrl) {
				p->sel_a = 0;
				p->sel_b = (int)text_items(p).size();
				InvalidateRect(hwnd, nullptr, FALSE);
			}
			return 0;
		}
		break;
	}
	case WM_LBUTTONDOWN: {
		SetFocus(hwnd);
		POINT pt = client_pt(lp);
		if (p->hover_mermaid && p->over_view) {
			open_mermaid(p, p->hover_pre);
			return 0;
		}
		if (p->hover_pre && p->over_btn) {
			copy_code(p, p->hover_pre);
			p->copied = true;
			SetTimer(hwnd, 1, 1200, nullptr);
			InvalidateRect(hwnd, nullptr, FALSE);
			return 0;
		}
		SetCapture(hwnd);
		p->down_pt = pt;
		p->dragging = true;
		p->selecting = false;
		if (p->sel_a >= 0) {
			p->sel_a = p->sel_b = -1;
			InvalidateRect(hwnd, nullptr, FALSE);
		}
		if (p->doc) {
			float s = p->scale();
			position::vector boxes;
			p->doc->on_lbutton_down(pt.x / s + p->sx, pt.y / s + p->sy, pt.x / s, pt.y / s, boxes);
			invalidate_boxes(p, boxes);
		}
		return 0;
	}
	case WM_MOUSEMOVE: {
		POINT pt = client_pt(lp);
		float s = p->scale();
		if (p->dragging && p->doc) {
			if (!p->selecting && (abs(pt.x - p->down_pt.x) > 3 || abs(pt.y - p->down_pt.y) > 3)) {
				p->selecting = true;
				position::vector boxes;
				p->doc->on_button_cancel(boxes);
				p->sel_a = caret_at(p, p->down_pt.x / s + p->sx, p->down_pt.y / s + p->sy);
			}
			if (p->selecting) {
				// Auto-scroll while dragging past the edges.
				if (pt.y < 0)
					scroll_to(p, p->sx, p->sy - 20, true);
				else if (pt.y > (LONG)(p->c.vh * s))
					scroll_to(p, p->sx, p->sy + 20, true);
				p->sel_b = caret_at(p, pt.x / s + p->sx, pt.y / s + p->sy);
				InvalidateRect(hwnd, nullptr, FALSE);
				return 0;
			}
		}
		TRACKMOUSEEVENT tme{sizeof(tme), TME_LEAVE, hwnd, 0};
		TrackMouseEvent(&tme);
		if (p->doc) {
			position::vector boxes;
			p->doc->on_mouse_over(pt.x / s + p->sx, pt.y / s + p->sy, pt.x / s, pt.y / s, boxes);
			invalidate_boxes(p, boxes);
			update_hover(p, pt);
		}
		return 0;
	}
	case WM_TIMER:
		if (wp == 1) {
			KillTimer(hwnd, 1);
			p->copied = false;
			InvalidateRect(hwnd, nullptr, FALSE);
			return 0;
		}
		break;
	case WM_MOUSELEAVE:
		clear_hover(p);
		if (p->doc) {
			position::vector boxes;
			p->doc->on_mouse_leave(boxes);
			invalidate_boxes(p, boxes);
		}
		p->c.cursor.clear();
		return 0;
	case WM_LBUTTONUP: {
		if (GetCapture() == hwnd)
			ReleaseCapture();
		bool was_selecting = p->selecting;
		p->dragging = p->selecting = false;
		if (was_selecting || !p->doc)
			return 0;
		POINT pt = client_pt(lp);
		float s = p->scale();
		p->c.clicked.clear();
		position::vector boxes;
		p->doc->on_lbutton_up(pt.x / s + p->sx, pt.y / s + p->sy, pt.x / s, pt.y / s, boxes);
		invalidate_boxes(p, boxes);
		if (!p->c.clicked.empty()) {
			std::string href = p->c.clicked;
			p->c.clicked.clear();
			SendMessageW(GetParent(hwnd), PVN_LINK, 0, (LPARAM)href.c_str());
		}
		return 0;
	}
	case WM_CAPTURECHANGED:
		p->dragging = p->selecting = false;
		return 0;
	case WM_LBUTTONDBLCLK: {
		element::ptr el = element_at(p, client_pt(lp));
		if (auto img = ancestor(el, {"img"}))
			open_image(p, img);
		else if (auto pre = ancestor(el, {"pre"}); pre && is_mermaid(pre))
			open_mermaid(p, pre);
		return 0;
	}
	case WM_SETCURSOR:
		if (LOWORD(lp) == HTCLIENT) {
			SetCursor(LoadCursorW(nullptr, p->over_btn || p->over_view || p->c.cursor == "pointer" ? IDC_HAND : IDC_ARROW));
			return TRUE;
		}
		break;
	case WM_CONTEXTMENU:
		context_menu(p, {GET_X_LPARAM(lp), GET_Y_LPARAM(lp)});
		return 0;
	}
	return DefWindowProcW(hwnd, msg, wp, lp);
}

// ---------------------------------------------------------------- C API

extern "C" HWND pv_create(HWND parent) {
	init_factories();
	static bool registered = false;
	HINSTANCE inst = GetModuleHandleW(nullptr);
	if (!registered) {
		WNDCLASSEXW wc{sizeof(wc)};
		wc.style = CS_DBLCLKS;
		wc.lpfnWndProc = preview_proc;
		wc.hInstance = inst;
		wc.hCursor = LoadCursorW(nullptr, IDC_ARROW);
		wc.lpszClassName = L"MDelightPreview";
		RegisterClassExW(&wc);
		registered = true;
	}
	return CreateWindowExW(0, L"MDelightPreview", L"", WS_CHILD | WS_VSCROLL | WS_HSCROLL | WS_TABSTOP, 0, 0, 100, 100, parent, nullptr, inst, nullptr);
}

/// Replace the document. `bg` is 0xRRGGBB; `keep_scroll` keeps the reading position across edits.
extern "C" void pv_set_document(HWND h, const char* html, const char* css, const wchar_t* base_dir, unsigned bg, unsigned sel, int keep_scroll) {
	Preview* p = get(h);
	if (!p)
		return;
	p->bg = D2D1::ColorF(((bg >> 16) & 255) / 255.f, ((bg >> 8) & 255) / 255.f, (bg & 255) / 255.f);
	p->sel = D2D1::ColorF(((sel >> 16) & 255) / 255.f, ((sel >> 8) & 255) / 255.f, (sel & 255) / 255.f, 0.45f);
	p->c.base_dir = base_dir ? base_dir : L"";
	p->html = html ? html : "";
	p->css = css ? css : "";
	p->c.cursor.clear();
	update_viewport(p);
	p->doc = document::createFromString(estring(p->html, encoding::utf_8), &p->c, litehtml::master_css, p->css);
	p->sel_a = p->sel_b = -1;
	clear_hover(p);
	if (!keep_scroll) {
		p->sx = p->sy = 0;
	}
	layout(p, true);
	InvalidateRect(h, nullptr, FALSE);
}

extern "C" float pv_scroll_y(HWND h) {
	Preview* p = get(h);
	return p ? p->sy : 0;
}

extern "C" void pv_set_scroll_y(HWND h, float y) {
	if (Preview* p = get(h))
		scroll_to(p, p->sx, y, false);
}

extern "C" float pv_view_height(HWND h) {
	Preview* p = get(h);
	return p ? p->c.vh : 0;
}

extern "C" float pv_content_height(HWND h) {
	Preview* p = get(h);
	return p && p->doc ? p->doc->height() : 0;
}

static bool is_anchor_tag(const char* t) {
	static const char* tags[] = {"p", "h1", "h2", "h3", "h4", "h5", "h6", "li", "pre", "blockquote", "table", "tr", "hr", "div"};
	for (auto x : tags)
		if (_stricmp(t, x) == 0)
			return true;
	return false;
}

static void collect_anchors(const element::ptr& el, std::vector<PvAnchor>& out) {
	if (el->is_text())
		return;
	const char* sp = el->get_attr("data-sourcepos");
	const char* tag = el->get_tagName();
	if (sp && tag && is_anchor_tag(tag)) {
		if (auto ri = el->get_render_item())
			out.push_back({atoi(sp), ri->get_placement().y});
	}
	for (auto& ch : el->children())
		collect_anchors(ch, out);
}

/// Source line → y offset pairs, in document order. Returns the total count; fills up to `cap`.
extern "C" int pv_anchors(HWND h, PvAnchor* out, int cap) {
	Preview* p = get(h);
	if (!p || !p->doc)
		return 0;
	std::vector<PvAnchor> v;
	collect_anchors(p->doc->root(), v);
	for (int i = 0; i < (int)v.size() && i < cap; i++)
		out[i] = v[i];
	return (int)v.size();
}

static element::ptr find_id(const element::ptr& el, const char* id) {
	if (el->is_text())
		return nullptr;
	const char* v = el->get_attr("id");
	if (v && strcmp(v, id) == 0)
		return el;
	for (auto& ch : el->children())
		if (auto r = find_id(ch, id))
			return r;
	return nullptr;
}

/// Top offset of the element with this id, or -1.
extern "C" float pv_find_id(HWND h, const char* id) {
	Preview* p = get(h);
	if (!p || !p->doc)
		return -1;
	auto el = find_id(p->doc->root(), id);
	if (!el)
		return -1;
	auto ri = el->get_render_item();
	return ri ? ri->get_placement().y : -1;
}

extern "C" void pv_clear(HWND h) {
	if (Preview* p = get(h)) {
		p->doc.reset();
		p->items.clear();
		p->items_dirty = true;
		p->sel_a = p->sel_b = -1;
		clear_hover(p);
		p->sx = p->sy = 0;
		update_scrollbars(p);
		InvalidateRect(h, nullptr, FALSE);
	}
}

extern "C" void pv_set_background(HWND h, unsigned bg) {
	if (Preview* p = get(h)) {
		p->bg = D2D1::ColorF(((bg >> 16) & 255) / 255.f, ((bg >> 8) & 255) / 255.f, (bg & 255) / 255.f);
		InvalidateRect(h, nullptr, FALSE);
	}
}
