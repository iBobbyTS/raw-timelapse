//! Private line-delimited JSON channel for the Qt preview controller.

use crate::cli::{Parsed, ReferenceGain};
use crate::color_profile::{self, OutputCatSelection};
use crate::error::{Error, Result, EXIT_USAGE};
use crate::frame::{self, PreparedFrame};
use crate::json::{self, Json};
use crate::lens;
use crate::order::SourceFrame;
use crate::proc;
use crate::output_color::{self, ResolvedOutputColor, AUTO_LOG};
use rawtl_linear_video::export::SOFTWARE_PROFILES;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const PREVIEW_EDGE: u32 = 1440;

struct Tools {
    native: PathBuf,
    exiftool: PathBuf,
    converter: Option<String>,
}

struct Scratch(PathBuf);

impl Scratch {
    fn create(path: PathBuf) -> Result<Self> {
        fs::create_dir(&path).map_err(io_error)?;
        Ok(Self(path))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Prepared {
    frame: PreparedFrame,
    _scratch: Scratch,
    lens: Json,
}

struct LinearFrame {
    index: usize,
    white_balance: bool,
    lens: bool,
    width: u32,
    height: u32,
    pixels: Arc<[u8]>,
    lens_status: Json,
}

struct Session {
    sources: Vec<SourceFrame>,
    settings: frame::PreparedSettings,
    auto_log: Result<&'static color_profile::ColorProfile>,
    first: Option<Prepared>,
    current: Option<(usize, Prepared)>,
    linear: Option<LinearFrame>,
}

impl Session {
    fn prepare(&self, index: usize, root: &Path, tools: &Tools) -> Result<Prepared> {
        let scratch = Scratch::create(root.join(format!("frame-{index}")))?;
        let mut lens_settings = self.settings.lens.clone();
        lens_settings.crop = lens::Crop::Preserve;
        let (mut prepared, _) = frame::prepare_export_active(
            &tools.native,
            std::slice::from_ref(&self.sources[index]),
            &frame::PrepareSequenceSettings {
                scratch_dir: &scratch.0,
                converter: tools.converter.as_deref(),
                converter_timeout: None,
                lens: lens_settings,
            },
            Default::default(),
            false,
            true,
            None,
        )?;
        let mut prepared = prepared.pop().ok_or_else(|| frame::failure("显影准备未返回帧"))?;
        let lens = prepared.preview_lens.take().ok_or_else(|| frame::failure("显影准备未返回镜头状态"))?;
        Ok(Prepared {
            frame: prepared,
            _scratch: scratch,
            lens,
        })
    }

    fn develop(
        &mut self,
        index: usize,
        freeze_wb: bool,
        correct_lens: bool,
        root: &Path,
        tools: &Tools,
    ) -> Result<()> {
        if self.linear.as_ref().is_some_and(|frame| {
            (frame.index, frame.white_balance, frame.lens) == (index, freeze_wb, correct_lens)
        }) {
            return Ok(());
        }
        if (index == 0 || freeze_wb) && self.first.is_none() {
            self.first = Some(self.prepare(0, root, tools)?);
        }
        if index != 0 && self.current.as_ref().map(|(i, _)| *i) != Some(index) {
            self.current = None;
            self.current = Some((index, self.prepare(index, root, tools)?));
        }
        let prepared = if index == 0 {
            self.first.as_ref().unwrap()
        } else {
            &self.current.as_ref().unwrap().1
        };
        let wb = if freeze_wb {
            self.first.as_ref().map(|p| p.frame.as_shot_neutral)
        } else {
            None
        };
        let mut lens_settings = self.settings.lens.clone();
        if !correct_lens {
            lens_settings.mode = lens::Mode::Off;
        }
        let settings = frame::PreparedSettings {
            camera_recipe: self.settings.camera_recipe,
            lens: lens_settings,
            preview_edge: Some(PREVIEW_EDGE),
        };
        let mut preview = None;
        frame::develop_prepared_at(
            &tools.native,
            &prepared.frame,
            wb,
            0.,
            &settings,
            index,
            self.sources.len(),
            &prepared._scratch.0.join("develop.log"),
            Default::default(),
            false,
            &mut |info, stream| {
                if info.width.max(info.height) > PREVIEW_EDGE {
                    return Err(frame::failure("预览画面超出分辨率上限"));
                }
                preview = Some(LinearFrame {
                    index,
                    white_balance: freeze_wb,
                    lens: correct_lens,
                    width: info.width,
                    height: info.height,
                    pixels: stream.pixels(info.width, info.height)?,
                    lens_status: prepared.lens.clone(),
                });
                Ok(())
            },
        )?;
        self.linear = preview;
        Ok(())
    }

    fn preview(&mut self, request: &Json, root: &Path, tools: &Tools) -> Result<Json> {
        let index = request
            .get("index")
            .and_then(Json::as_u64)
            .and_then(|i| usize::try_from(i).ok())
            .filter(|&i| i < self.sources.len())
            .ok_or_else(|| usage("帧编号超出范围"))?;
        let freeze_wb = match required_text(request, "white_balance")? {
            "first-frame" => true,
            "per-frame" => false,
            _ => return Err(usage("无效的白平衡模式")),
        };
        let correct_lens = request
            .get("lens")
            .and_then(Json::as_bool)
            .ok_or_else(|| usage("缺少镜头校正选项"))?;
        let id = required_text(request, "color")?;
        let profile = if id == AUTO_LOG {
            self.auto_log.as_ref().copied().map_err(Clone::clone)?
        } else {
            color_profile::lookup(id).ok_or_else(|| usage("未知色彩配置"))?
        };
        let color = ResolvedOutputColor::new(profile, OutputCatSelection::Profile, None)?;
        let gain = match color.reference_gain {
            ReferenceGain::Fixed(gain) => gain,
            ReferenceGain::Auto => profile.transfer.desired_auto_reference_gain(),
        };
        self.develop(index, freeze_wb, correct_lens, root, tools)?;
        let image = self
            .linear
            .as_ref()
            .ok_or_else(|| frame::failure("显影未返回画面"))?;
        let (signal, _) = profile
            .convert_f32_frame_to_target_rgb(
                &color.transform,
                &image.pixels,
                image.width as usize,
                image.height as usize,
                gain,
            )
            .map_err(frame::failure)?;
        let output = root.join("preview.ppm");
        let mut file = io::BufWriter::new(fs::File::create(&output).map_err(io_error)?);
        write!(file, "P6\n{} {}\n255\n", image.width, image.height).map_err(io_error)?;
        let data: Vec<u8> = signal
            .iter()
            .flatten()
            .map(|value| (value.clamp(0., 1.) * 255.).round() as u8)
            .collect();
        file.write_all(&data)
            .and_then(|_| file.flush())
            .map_err(io_error)?;
        Ok(Json::obj(vec![
            ("image", Json::Str(path_text(&output)?)),
            ("name", Json::Str(self.sources[index].name.clone())),
            ("lens", image.lens_status.clone()),
        ]))
    }
}

struct Worker {
    root: PathBuf,
    tools: Tools,
    session: Option<Session>,
}

impl Worker {
    fn dispatch(&mut self, request: &Json) -> Result<Json> {
        match required_text(request, "command")? {
            "hello" => Ok(Json::obj(vec![
                (
                    "profile",
                    json::parse(include_str!("default_profile.json")).map_err(usage)?,
                ),
                (
                    "colors",
                    Json::Array(
                        color_profile::PROFILES
                            .iter()
                            .map(|profile| Json::Str(profile.id.into()))
                            .collect(),
                    ),
                ),
                (
                    "export_presets",
                    Json::Array(
                        SOFTWARE_PROFILES.into_iter().map(|name| Json::Str(name.into())).collect(),
                    ),
                ),
            ])),
            "open" => self.open(required_text(request, "path")?),
            "close" => {
                self.session = None;
                let _ = fs::remove_file(self.root.join("preview.ppm"));
                Ok(Json::Null)
            }
            "preview" => self
                .session
                .as_mut()
                .ok_or_else(|| usage("请先打开素材"))?
                .preview(request, &self.root, &self.tools),
            _ => Err(usage("未知桌面请求")),
        }
    }

    fn open(&mut self, input: &str) -> Result<Json> {
        let input = crate::guard::canonical_input(Path::new(input))?;
        let input_path = path_text(&input)?;
        let sources = crate::order::enumerate_dng(&input)?;
        let Parsed::Run(options) =
            crate::profile::parse(&[format!("--input={input_path}"), "--profile=default".into()])?
        else {
            unreachable!("desktop requests a processing profile")
        };
        let all_cr3 = sources.iter().all(|source| {
            source
                .path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("cr3"))
        });
        let camera_recipe = options.camera_color.resolve(all_cr3).map_err(usage)?;
        let auto_log =
            output_color::detect(&sources[0], &self.tools.exiftool).map(|(profile, _, _)| profile);
        let result = Json::obj(vec![
            ("path", Json::Str(input_path)),
            ("output", Json::Str(path_text(&options.video)?)),
            ("frames", Json::Int(sources.len() as i64)),
            (
                "auto_log",
                auto_log
                    .as_ref()
                    .map_or(Json::Null, |profile| Json::Str(profile.id.into())),
            ),
            (
                "auto_error",
                auto_log
                    .as_ref()
                    .err()
                    .map_or(Json::Null, |err| Json::Str(err.message.clone())),
            ),
        ]);
        self.session = Some(Session {
            sources,
            auto_log,
            first: None,
            current: None,
            linear: None,
            settings: frame::PreparedSettings {
                camera_recipe,
                preview_edge: Some(PREVIEW_EDGE),
                lens: lens::Settings {
                    exiftool: self.tools.exiftool.clone(),
                    ..options.lens
                },
            },
        });
        let _ = fs::remove_file(self.root.join("preview.ppm"));
        Ok(result)
    }
}

fn io_error(error: io::Error) -> Error {
    frame::failure(error.to_string())
}

fn usage(message: impl Into<String>) -> Error {
    Error::new(EXIT_USAGE, message)
}

fn path_text(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| usage("路径不是有效的 UTF-8"))
}

fn required_text<'a>(value: &'a Json, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Json::as_str)
        .ok_or_else(|| usage(format!("缺少字段 {key}")))
}

pub fn serve() -> Result<()> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if let [flag, ffmpeg] = arguments.as_slice() {
        if flag == "--hardware-presets" {
            let profiles = crate::video::available_hardware_profiles(Path::new(ffmpeg))?;
            let result = profiles
                .into_iter()
                .map(|profile| {
                    Json::obj(vec![
                        ("variant", Json::Str(profile.hardware_variant().expect("discovered layout"))),
                        ("bits", Json::Int(i64::from(profile.input.bits()))),
                        ("id", Json::Str(profile.name)),
                    ])
                })
                .collect();
            println!("{}", json::to_pretty_string(&Json::Array(result)));
            return Ok(());
        }
    }
    let [root, native, exiftool, converter] = arguments.as_slice() else {
        return Err(usage(
            "桌面预览进程需要工作目录、显影工具、ExifTool 和 Converter 路径",
        ));
    };
    let mut worker = Worker {
        root: PathBuf::from(root),
        tools: Tools {
            native: PathBuf::from(native),
            exiftool: PathBuf::from(exiftool),
            converter: (!converter.is_empty()).then(|| converter.clone()),
        },
        session: None,
    };
    let mut stdout = io::stdout().lock();
    for line in io::stdin().lock().lines() {
        proc::check_stop()?;
        let reply = match json::parse(&line.map_err(io_error)?)
            .map_err(usage)
            .and_then(|request| worker.dispatch(&request))
        {
            Ok(result) => Json::obj(vec![("result", result)]),
            Err(error) => Json::obj(vec![("error", Json::Str(error.message))]),
        };
        writeln!(
            stdout,
            "{}",
            json::to_pretty_string(&reply).replace('\n', "")
        )
        .map_err(io_error)?;
        stdout.flush().map_err(io_error)?;
    }
    Ok(())
}
