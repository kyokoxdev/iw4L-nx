// Must write the same bytes as DekoCompiler::OutputDksh.
// Not thread-safe: Mesa keeps a global GL context.

#include "compiler_iface.h"

#include <vector>

struct UamShim
{
	static void Dksh(DekoCompiler& c, std::vector<uint8_t>& out)
	{
		auto put = [&](const void* p, size_t n) {
			auto b = static_cast<const uint8_t*>(p);
			out.insert(out.end(), b, b + n);
		};
		auto align = [&]() { out.resize((out.size() + 0xFF) & ~size_t(0xFF), 0); };
		auto align256 = [](uint32_t x) { return (x + 0xFF) & ~0xFFu; };
		bool graphics = c.m_stage != pipeline_stage_compute;

		DkshHeader hdr = {};
		hdr.magic        = DKSH_MAGIC;
		hdr.header_sz    = sizeof(DkshHeader);
		hdr.control_sz   = align256(sizeof(DkshHeader) + sizeof(DkshProgramHeader));
		hdr.code_sz      = align256((graphics ? 0x80 : 0x00) + c.m_codeSize) + align256(c.m_dataSize);
		hdr.programs_off = sizeof(DkshHeader);
		hdr.num_programs = 1;

		put(&hdr, sizeof(hdr));
		put(&c.m_dkph, sizeof(c.m_dkph));
		align();
		if (graphics)
		{
			static const char s_padding[0x80 - sizeof(NvShaderHeader)] = "lol nvidia why did you make us waste space here";
			put(s_padding, sizeof(s_padding));
			put(&c.m_nvsh, sizeof(c.m_nvsh));
		}
		put(c.m_code, c.m_codeSize);
		align();
		if (c.m_dataSize)
		{
			put(c.m_data, c.m_dataSize);
			align();
		}
	}
};

// *out is malloc'd; free it with uam_free.
extern "C" int uam_compile(const char* glsl, int stage, uint8_t** out, size_t* out_len)
{
	if (stage < pipeline_stage_vertex || stage > pipeline_stage_compute)
		return 2;
	DekoCompiler compiler{static_cast<pipeline_stage>(stage)};
	if (!compiler.CompileGlsl(glsl))
		return 1;
	std::vector<uint8_t> dksh;
	UamShim::Dksh(compiler, dksh);
	*out = static_cast<uint8_t*>(malloc(dksh.size()));
	if (!*out)
		return 3;
	memcpy(*out, dksh.data(), dksh.size());
	*out_len = dksh.size();
	return 0;
}

extern "C" void uam_free(uint8_t* p)
{
	free(p);
}
