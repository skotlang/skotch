; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.str.0 = private unnamed_addr constant [3 x i8] c"x=\00", align 1
@.str.1 = private unnamed_addr constant [3 x i8] c"b=\00", align 1
@.str.2 = private unnamed_addr constant [9 x i8] c"answer: \00", align 1
@.str.3 = private unnamed_addr constant [10 x i8] c", valid: \00", align 1
@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @puts(ptr)
@.fmt.concat.ss = private unnamed_addr constant [5 x i8] c"%s%s\00", align 1
@.fmt.concat.sd = private unnamed_addr constant [5 x i8] c"%s%d\00", align 1
declare i32 @snprintf(ptr, i32, ptr, ...)

define i32 @main() {
entry:
  %t0 = add i32 0, 42
  %t1 = add i32 0, 1
  %t2 = alloca [256 x i8]
  call i32 (ptr, i32, ptr, ...) @snprintf(ptr %t2, i32 256, ptr @.fmt.concat.sd, ptr @.str.0, i32 %t0)
  call i32 @puts(ptr %t2)
  %t5 = alloca [256 x i8]
  call i32 (ptr, i32, ptr, ...) @snprintf(ptr %t5, i32 256, ptr @.fmt.concat.ss, ptr @.str.1, ptr %t1)
  call i32 @puts(ptr %t5)
  %t8 = alloca [256 x i8]
  call i32 (ptr, i32, ptr, ...) @snprintf(ptr %t8, i32 256, ptr @.fmt.concat.sd, ptr @.str.2, i32 %t0)
  %t10 = alloca [256 x i8]
  call i32 (ptr, i32, ptr, ...) @snprintf(ptr %t10, i32 256, ptr @.fmt.concat.ss, ptr %t8, ptr @.str.3)
  %t12 = alloca [256 x i8]
  call i32 (ptr, i32, ptr, ...) @snprintf(ptr %t12, i32 256, ptr @.fmt.concat.ss, ptr %t10, ptr %t1)
  call i32 @puts(ptr %t12)
  ret i32 0
}

