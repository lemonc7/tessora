use topcoat::{
    Result,
    context::Cx,
    runtime::{Event, Signal, signal},
    view::{View, attributes, component, view},
};

use crate::components::{
    button::button,
    card::{card, card_content, card_description, card_header, card_title},
    input::input,
    select::select,
};

use super::{actions::create_download, manager};

#[component]
pub(crate) async fn download_form_view(cx: &Cx, notice: Signal<String>) -> Result<impl View> {
    let directories = manager(cx).list_directories()?;
    let url = signal(cx, String::new);
    let directory = signal(cx, String::new);
    let file_name = signal(cx, String::new);
    let submitting = signal(cx, || false);

    Ok(view! {
        card(
            card_header(
                card_title("新建下载")
                card_description(
                    "选择下载目录并输入文件名；留空文件名时自动生成。"
                )
            )
            card_content(
                <div class="grid gap-4">
                    <label class="grid gap-2">
                        <span class="text-sm font-medium">"M3U8 地址"</span>
                        input(
                            attrs: attributes! {
                                type="url"
                                placeholder="https://example.com/video.m3u8"
                                :value=$(url.get())
                                @input=$(|event: Event| url.set(event.target.value))
                            }
                        )
                    </label>

                    <div class="grid gap-4 md:grid-cols-2">
                        <label class="grid gap-2">
                            <span class="text-sm font-medium">"下载目录"</span>
                            select(
                                attrs: attributes! {
                                    aria-label="下载目录"
                                    :value=$(directory.get())
                                    @change=$(|event: Event| directory.set(event.target.value))
                                },
                                for entry in directories {
                                    <option value=(&entry)>
                                        if entry.is_empty() {
                                            "下载根目录"
                                        } else {
                                            (&entry)
                                        }
                                    </option>
                                }
                            )
                        </label>

                        <label class="grid gap-2">
                            <span class="text-sm font-medium">
                                "文件名（无需后缀）"
                            </span>
                            <div
                                class="flex rounded-lg focus-within:ring-2 focus-within:ring-ring focus-within:ring-offset-2 focus-within:ring-offset-background"
                            >
                                input(
                                    attrs: attributes! {
                                        class="rounded-r-none focus-visible:ring-0 focus-visible:ring-offset-0"
                                        placeholder="第 01 集"
                                        :value=$(file_name.get())
                                        @input=$(|event: Event| file_name.set(event.target.value))
                                    }
                                )
                                <span
                                    class="inline-flex h-9 items-center rounded-r-lg border border-l-0 border-border bg-foreground/5 px-3 text-sm text-muted-foreground"
                                >
                                    ".mp4"
                                </span>
                            </div>
                        </label>
                    </div>

                    <div>
                        button(
                            attrs: attributes! {
                                type="button"
                                :disabled=$(submitting.get())
                                @click=$(async |_event: Event| {
                                    submitting.set(true);
                                    let result = create_download(
                                        url.get(),
                                        directory.get(),
                                        file_name.get(),
                                    ).await;

                                    notice.set(result.1);
                                    if result.0 {
                                        url.set("".to_owned());
                                        file_name.set("".to_owned());
                                    }
                                    submitting.set(false);
                                })
                            },
                            "开始下载"
                        )
                    </div>
                </div>
            )
        )
    })
}
